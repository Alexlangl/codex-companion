use crate::account_refresh::{
    provider_supports_api_key_usage, refresh_api_key_usage, refresh_official_codex_account,
};
use crate::auth::resolve_auth_token;
use chrono::Utc;
use codex_companion_core::{
    append_diagnostic_log, provider_api_base_url, CompanionError, ConfigStore, ProviderConfig,
    ProviderHealth, ProviderKind, Result,
};
use codex_companion_health::{classify_failure, mark_failure, mark_success};

#[derive(Debug, Clone)]
pub struct ProviderTestFailure {
    pub status: Option<u16>,
    pub message: String,
}

impl ProviderTestFailure {
    fn network(message: String) -> Self {
        Self {
            status: None,
            message,
        }
    }
}

pub async fn test_provider(provider: &ProviderConfig) -> std::result::Result<(), String> {
    test_provider_detailed(provider)
        .await
        .map_err(|failure| failure.message)
}

pub async fn test_provider_detailed(
    provider: &ProviderConfig,
) -> std::result::Result<(), ProviderTestFailure> {
    if provider.kind == ProviderKind::OfficialCodex {
        return refresh_official_codex_account(provider)
            .await
            .map(|_| ())
            .map_err(|error| ProviderTestFailure::network(error.to_string()));
    }

    let client = reqwest::Client::new();
    let url = format!(
        "{}/models",
        provider_api_base_url(&provider.base_url).trim_end_matches('/')
    );
    let mut request = client.get(url);
    if let Some(token) = resolve_auth_token(provider) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| ProviderTestFailure::network(format!("network failed: {error}")))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(ProviderTestFailure {
            status: Some(status.as_u16()),
            message: format!("provider returned {status}: {body}"),
        })
    }
}

pub async fn refresh_provider_status(store: &ConfigStore, id: &str) -> Result<ProviderHealth> {
    let config = store.load()?;
    let provider = config
        .providers
        .get(id)
        .cloned()
        .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown provider: {id}")))?;
    let mut account_result = None;
    let mut api_usage_error = None;
    let result = if provider.kind == ProviderKind::OfficialCodex {
        let result = refresh_official_codex_account(&provider).await;
        let health_result = result
            .as_ref()
            .map(|_| ())
            .map_err(|error| ProviderTestFailure::network(error.to_string()));
        account_result = Some(result);
        health_result
    } else if provider_supports_api_key_usage(&provider) {
        match refresh_api_key_usage(&provider).await {
            Ok(account) => {
                account_result = Some(Ok(account));
                Ok(())
            }
            Err(error) => {
                api_usage_error = Some(error.to_string());
                test_provider_detailed(&provider).await
            }
        }
    } else {
        test_provider_detailed(&provider).await
    };
    if let Some(error) = api_usage_error.as_deref() {
        let _ = append_diagnostic_log(
            &store.data_dir(),
            "warn",
            "provider",
            &format!("Provider {id} 额度刷新失败: {error}"),
        );
    }
    store.update(|config| {
        let now = Utc::now();
        let health = config.health.entry(id.to_string()).or_default();
        health.last_refresh_attempt = Some(now);
        match result {
            Ok(()) => {
                mark_success(health);
                if let Some(provider) = config.providers.get_mut(id) {
                    if let Some(Ok(account)) = account_result {
                        provider.account = Some(account);
                    } else if api_usage_error.is_some() {
                        let mut account = provider.account.clone().unwrap_or_default();
                        apply_usage_refresh_failure(&mut account, &provider.name, now);
                        provider.account = Some(account);
                    } else if let Some(account) = provider.account.as_mut() {
                        if provider.kind != ProviderKind::OfficialCodex {
                            clear_api_key_usage(account);
                            account.subscription_status = Some("连接正常".to_string());
                        }
                        account.last_refresh_at = Some(now.to_rfc3339());
                    }
                }
            }
            Err(failure) => {
                let classification = classify_failure(failure.status, &failure.message);
                mark_failure(health, &classification, failure.message);
            }
        }
        Ok(health.clone())
    })
}

fn apply_usage_refresh_failure(
    account: &mut codex_companion_core::ProviderAccountInfo,
    provider_name: &str,
    now: chrono::DateTime<Utc>,
) {
    account.display_name = account
        .display_name
        .clone()
        .or_else(|| Some(provider_name.to_string()));
    account.subscription_type = account
        .subscription_type
        .clone()
        .or_else(|| Some("API Key".to_string()));
    if has_usage_snapshot(account) {
        if account
            .subscription_status
            .as_deref()
            .is_none_or(|status| status.contains("额度刷新失败"))
        {
            account.subscription_status = Some("连接正常".to_string());
        }
    } else {
        clear_api_key_usage(account);
        account.subscription_status = Some("连接正常".to_string());
        account.last_refresh_at = Some(now.to_rfc3339());
    }
}

fn has_usage_snapshot(account: &codex_companion_core::ProviderAccountInfo) -> bool {
    account.usage_total.is_some()
        || account.usage_used.is_some()
        || account.usage_available.is_some()
        || account.quota_percent.is_some()
        || !account.quota_windows.is_empty()
}

fn clear_api_key_usage(account: &mut codex_companion_core::ProviderAccountInfo) {
    account.quota_label = None;
    account.quota_percent = None;
    account.quota_reset_at = None;
    account.quota_windows.clear();
    account.usage_total = None;
    account.usage_used = None;
    account.usage_available = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{
        default_refresh_interval_seconds, HealthFailureKind, HealthStatusKind, ProviderAccountInfo,
        ProviderQuotaWindow,
    };

    #[tokio::test]
    async fn upstream_401_marks_provider_auth_failed() {
        use axum::{http::StatusCode, routing::any, Router};
        let app = Router::new().route(
            "/{*path}",
            any(|| async {
                (
                    StatusCode::UNAUTHORIZED,
                    r#"{"error":{"message":"invalid key"}}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert(
                    "p".to_string(),
                    ProviderConfig {
                        id: "p".to_string(),
                        name: "p".to_string(),
                        kind: ProviderKind::OpenAiCompatible,
                        base_url: format!("http://{addr}/v1"),
                        websocket_url: None,
                        auth_ref: None,
                        direct_auth_ref: None,
                        model_map: std::collections::BTreeMap::new(),
                        priority: 0,
                        enabled: true,
                        refresh_interval_seconds: default_refresh_interval_seconds(),
                        account: None,
                    },
                );
                Ok(())
            })
            .expect("seed provider");

        let health = refresh_provider_status(&store, "p").await.expect("refresh");

        // 401 是凭据失效，必须归类为 AuthFailed 而不是网络故障，
        // 否则代理层无法把吊销的 key 从候选里剔除。
        assert_eq!(health.status, HealthStatusKind::AuthFailed);
        assert_eq!(
            health.last_failure_kind,
            Some(HealthFailureKind::AuthFailed)
        );
        assert!(health.last_refresh_attempt.is_some());
    }

    #[test]
    fn transient_usage_failure_preserves_last_successful_snapshot() {
        let refreshed_at = "2026-07-18T08:00:00Z".to_string();
        let mut account = ProviderAccountInfo {
            quota_label: Some("本月剩余".to_string()),
            quota_percent: Some(75.0),
            quota_windows: vec![ProviderQuotaWindow {
                label: "月".to_string(),
                remaining_percent: 75.0,
                reset_at: None,
                window_minutes: None,
            }],
            usage_total: Some(100.0),
            usage_used: Some(25.0),
            usage_available: Some(75.0),
            last_refresh_at: Some(refreshed_at.clone()),
            ..ProviderAccountInfo::default()
        };

        apply_usage_refresh_failure(&mut account, "Provider", Utc::now());

        assert_eq!(account.usage_available, Some(75.0));
        assert_eq!(account.quota_percent, Some(75.0));
        assert_eq!(account.quota_windows.len(), 1);
        assert_eq!(account.quota_label.as_deref(), Some("本月剩余"));
        assert_eq!(
            account.last_refresh_at.as_deref(),
            Some(refreshed_at.as_str())
        );
        assert_eq!(account.subscription_status.as_deref(), Some("连接正常"));
    }

    #[test]
    fn first_usage_failure_does_not_invent_a_snapshot() {
        let mut account = ProviderAccountInfo::default();
        apply_usage_refresh_failure(&mut account, "Provider", Utc::now());

        assert_eq!(account.quota_label, None);
        assert!(account.last_refresh_at.is_some());
        assert!(!has_usage_snapshot(&account));
    }
}
