use crate::account_refresh::{
    provider_supports_api_key_usage, refresh_api_key_usage, refresh_official_codex_account,
};
use crate::agent_identity::{ensure_agent_identity_authorization, provider_uses_agent_identity};
use crate::auth::resolve_auth_token;
use crate::codex_oauth::{ensure_codex_auth_snapshot_detailed, provider_uses_codex_oauth};
use crate::http::read_response_bytes_limited;
use chrono::Utc;
use codex_companion_core::{
    append_diagnostic_log, http_client_builder, provider_api_base_url, redact_sensitive_text,
    CompanionError, ConfigStore, HealthFailureKind, ProviderConfig, ProviderHealth, ProviderKind,
    Result,
};

const PROVIDER_TEST_RESPONSE_LIMIT_BYTES: usize = 128 * 1024;
use codex_companion_health::{
    classification_for_kind, classify_failure, mark_failure, mark_success, FailureClassification,
};

#[derive(Debug, Clone)]
pub struct ProviderTestFailure {
    pub status: Option<u16>,
    pub message: String,
    pub classification: Option<FailureClassification>,
}

impl ProviderTestFailure {
    fn network(message: String) -> Self {
        Self {
            status: None,
            message,
            classification: None,
        }
    }

    fn auth(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
            classification: Some(classification_for_kind(HealthFailureKind::AuthFailed)),
        }
    }

    fn oauth(error: crate::codex_oauth::CodexOAuthError) -> Self {
        let classification = error.failure_classification();
        Self {
            status: error.status,
            message: error.message,
            classification: Some(classification),
        }
    }
}

#[derive(Debug, Clone)]
struct ProviderRefreshSnapshot {
    provider: ProviderConfig,
    api_key: Option<Option<String>>,
    usage_credentials: Option<Option<Vec<u8>>>,
}

impl ProviderRefreshSnapshot {
    fn capture(store: &ConfigStore, id: &str) -> Result<Self> {
        let config = store.load()?;
        let provider = config
            .providers
            .get(id)
            .cloned()
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown provider: {id}")))?;
        let api_key = (!matches!(provider.kind, ProviderKind::OfficialCodex))
            .then(|| resolve_auth_token(&provider));
        let usage_credentials = provider
            .account
            .as_ref()
            .and_then(|account| account.usage_query.as_ref())
            .map(|_| std::fs::read(crate::registry::usage_query_credential_path(store, id)).ok());
        Ok(Self {
            provider,
            api_key,
            usage_credentials,
        })
    }

    fn is_current(&self, store: &ConfigStore, current: &ProviderConfig) -> bool {
        &self.provider == current
            && self
                .api_key
                .as_ref()
                .is_none_or(|expected| resolve_auth_token(current).as_ref() == expected.as_ref())
            && self.usage_credentials.as_ref().is_none_or(|expected| {
                std::fs::read(crate::registry::usage_query_credential_path(
                    store,
                    &current.id,
                ))
                .ok()
                .as_ref()
                    == expected.as_ref()
            })
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
        if provider_uses_agent_identity(provider) {
            let client = http_client_builder()
                .timeout(std::time::Duration::from_secs(15))
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(|error| {
                    ProviderTestFailure::network(format!(
                        "Agent Identity network client failed: {error}"
                    ))
                })?;
            return ensure_agent_identity_authorization(&client, provider, None)
                .await
                .map(|_| ())
                .map_err(|error| {
                    let message = error.to_string();
                    ProviderTestFailure {
                        status: None,
                        classification: Some(classify_failure(None, &message)),
                        message,
                    }
                });
        }
        if provider_uses_codex_oauth(provider) {
            return ensure_codex_auth_snapshot_detailed(provider)
                .await
                .map(|_| ())
                .map_err(ProviderTestFailure::oauth);
        }
        return resolve_auth_token(provider)
            .filter(|token| !token.trim().is_empty())
            .map(|_| ())
            .ok_or_else(|| ProviderTestFailure::auth("官方 PAT 缺少 access_token"));
    }

    let client = http_client_builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|error| ProviderTestFailure::network(format!("network client failed: {error}")))?;
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
        let body = read_response_bytes_limited(response, PROVIDER_TEST_RESPONSE_LIMIT_BYTES)
            .await
            .map(|body| redact_sensitive_text(&String::from_utf8_lossy(&body)))
            .unwrap_or_else(|error| format!("[{error}]"));
        Err(ProviderTestFailure {
            status: Some(status.as_u16()),
            message: format!("provider returned {status}: {body}"),
            classification: None,
        })
    }
}

pub async fn refresh_provider_status(store: &ConfigStore, id: &str) -> Result<ProviderHealth> {
    let snapshot = ProviderRefreshSnapshot::capture(store, id)?;
    let provider = snapshot.provider.clone();
    let mut account_result = None;
    let mut api_usage_error = None;
    let result = if provider.kind == ProviderKind::OfficialCodex {
        let health_result = test_provider_detailed(&provider).await;
        if health_result.is_ok() && provider_uses_codex_oauth(&provider) {
            match tokio::time::timeout(
                std::time::Duration::from_secs(35),
                refresh_official_codex_account(&provider),
            )
            .await
            {
                Ok(result) => match result {
                    Ok(account) => account_result = Some(account),
                    Err(error) => api_usage_error = Some(error.to_string()),
                },
                Err(_) => api_usage_error = Some("Codex 额度刷新超时".to_string()),
            }
        }
        health_result
    } else if provider_supports_api_key_usage(&provider) {
        match tokio::time::timeout(
            std::time::Duration::from_secs(35),
            refresh_api_key_usage(&provider, &store.data_dir()),
        )
        .await
        {
            Ok(Ok(account)) => {
                account_result = Some(account);
            }
            Ok(Err(error)) => {
                api_usage_error = Some(error.to_string());
            }
            Err(_) => api_usage_error = Some("额度刷新超时".to_string()),
        }
        // Usage endpoints may use different credentials and hosts. Their success must not
        // clear relay auth/rate-limit/network failures for the actual provider endpoint.
        test_provider_detailed(&provider).await
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
    persist_refresh_outcome(
        store,
        id,
        &snapshot,
        result,
        account_result,
        api_usage_error,
    )
}

fn persist_refresh_outcome(
    store: &ConfigStore,
    id: &str,
    snapshot: &ProviderRefreshSnapshot,
    result: std::result::Result<(), ProviderTestFailure>,
    account_result: Option<codex_companion_core::ProviderAccountInfo>,
    api_usage_error: Option<String>,
) -> Result<ProviderHealth> {
    store.update(|config| {
        let target_is_current = config
            .providers
            .get(id)
            .is_some_and(|current| snapshot.is_current(store, current));
        if !target_is_current {
            return Ok(config.health.get(id).cloned().unwrap_or_default());
        }
        let now = Utc::now();
        let health = config.health.entry(id.to_string()).or_default();
        health.last_refresh_attempt = Some(now);
        match result {
            Ok(()) => {
                mark_success(health);
                if let Some(provider) = config.providers.get_mut(id) {
                    if let Some(account) = account_result {
                        provider.account = Some(account);
                    } else if api_usage_error.is_some() {
                        preserve_usage_after_refresh_failure(provider, now);
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
                let classification = failure
                    .classification
                    .unwrap_or_else(|| classify_failure(failure.status, &failure.message));
                mark_failure(health, &classification, failure.message);
                if let Some(account) = account_result {
                    if let Some(provider) = config.providers.get_mut(id) {
                        provider.account = Some(account);
                    }
                }
            }
        }
        Ok(health.clone())
    })
}

fn preserve_usage_after_refresh_failure(provider: &mut ProviderConfig, now: chrono::DateTime<Utc>) {
    // Official quota endpoints are independent from relay traffic. Keep the last successful
    // account snapshot verbatim so a transient quota failure cannot overwrite useful data.
    if provider.kind == ProviderKind::OfficialCodex {
        return;
    }
    let mut account = provider.account.clone().unwrap_or_default();
    apply_usage_refresh_failure(&mut account, &provider.name, now);
    provider.account = Some(account);
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
    use base64::{engine::general_purpose, Engine as _};
    use codex_companion_core::{
        default_refresh_interval_seconds, HealthFailureKind, HealthStatusKind, ProviderAccountInfo,
        ProviderQuotaWindow,
    };
    use ed25519_dalek::{pkcs8::EncodePrivateKey, SigningKey};

    fn official_provider(auth_path: &std::path::Path) -> ProviderConfig {
        ProviderConfig {
            id: "official".to_string(),
            name: "Official".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: std::collections::BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    fn api_provider() -> ProviderConfig {
        ProviderConfig {
            id: "api".to_string(),
            name: "API".to_string(),
            kind: ProviderKind::RelayProvider,
            base_url: "https://relay.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: std::collections::BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: Some(ProviderAccountInfo::default()),
        }
    }

    #[tokio::test]
    async fn official_health_check_does_not_depend_on_quota_endpoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"opaque-valid-token"}}"#,
        )
        .expect("write auth");

        test_provider_detailed(&official_provider(&auth_path))
            .await
            .expect("valid local OAuth snapshot is healthy");
    }

    #[tokio::test]
    async fn agent_identity_health_check_builds_an_authorization_without_a_pat() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let private_key =
            general_purpose::STANDARD.encode(signing_key.to_pkcs8_der().expect("pkcs8").as_bytes());
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("agent.json");
        std::fs::write(
            &auth_path,
            serde_json::json!({
                "auth_mode": "agentIdentity",
                "agent_runtime_id": "runtime-1",
                "agent_private_key": private_key,
                "task_id": "task-1"
            })
            .to_string(),
        )
        .expect("agent auth");
        let mut provider = official_provider(&auth_path);
        provider.account = Some(ProviderAccountInfo {
            auth_mode: Some("agentIdentity".to_string()),
            ..ProviderAccountInfo::default()
        });

        test_provider_detailed(&provider)
            .await
            .expect("Agent Identity should use its local signing material");
    }

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

    #[test]
    fn official_usage_failure_preserves_last_successful_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut provider = official_provider(&temp.path().join("auth.json"));
        let account = ProviderAccountInfo {
            quota_label: Some("5h 100%".to_string()),
            quota_percent: Some(100.0),
            last_refresh_at: Some("2026-08-06T08:00:00Z".to_string()),
            ..ProviderAccountInfo::default()
        };
        provider.account = Some(account.clone());

        preserve_usage_after_refresh_failure(&mut provider, Utc::now());

        let preserved = provider.account.expect("account snapshot");
        assert_eq!(preserved.quota_label, account.quota_label);
        assert_eq!(preserved.quota_percent, account.quota_percent);
        assert_eq!(preserved.last_refresh_at, account.last_refresh_at);
        assert_eq!(preserved.subscription_status, account.subscription_status);
    }

    #[test]
    fn stale_refresh_does_not_recreate_deleted_provider_health() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = api_provider();
        store
            .update(|config| {
                config.providers.insert(provider.id.clone(), provider);
                Ok(())
            })
            .expect("seed provider");
        let snapshot = ProviderRefreshSnapshot::capture(&store, "api").expect("snapshot");
        store
            .update(|config| {
                config.providers.remove("api");
                config.health.remove("api");
                Ok(())
            })
            .expect("remove provider");

        persist_refresh_outcome(
            &store,
            "api",
            &snapshot,
            Ok(()),
            Some(ProviderAccountInfo {
                usage_available: Some(99.0),
                ..ProviderAccountInfo::default()
            }),
            None,
        )
        .expect("discard stale refresh");

        let config = store.load().expect("config");
        assert!(!config.providers.contains_key("api"));
        assert!(!config.health.contains_key("api"));
    }

    #[test]
    fn stale_refresh_does_not_overwrite_edited_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = api_provider();
        store
            .update(|config| {
                config.providers.insert(provider.id.clone(), provider);
                Ok(())
            })
            .expect("seed provider");
        let snapshot = ProviderRefreshSnapshot::capture(&store, "api").expect("snapshot");
        store
            .update(|config| {
                let provider = config.providers.get_mut("api").expect("provider");
                provider.name = "Edited while refreshing".to_string();
                provider.account = Some(ProviderAccountInfo {
                    usage_available: Some(42.0),
                    ..ProviderAccountInfo::default()
                });
                Ok(())
            })
            .expect("edit provider");

        persist_refresh_outcome(
            &store,
            "api",
            &snapshot,
            Ok(()),
            Some(ProviderAccountInfo {
                usage_available: Some(99.0),
                ..ProviderAccountInfo::default()
            }),
            None,
        )
        .expect("discard stale refresh");

        let config = store.load().expect("config");
        let provider = &config.providers["api"];
        assert_eq!(provider.name, "Edited while refreshing");
        assert_eq!(
            provider
                .account
                .as_ref()
                .and_then(|account| account.usage_available),
            Some(42.0)
        );
        assert!(!config.health.contains_key("api"));
    }

    #[test]
    fn successful_usage_refresh_is_kept_when_provider_health_check_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = api_provider();
        store
            .update(|config| {
                config.providers.insert(provider.id.clone(), provider);
                Ok(())
            })
            .expect("seed provider");
        let snapshot = ProviderRefreshSnapshot::capture(&store, "api").expect("snapshot");

        let health = persist_refresh_outcome(
            &store,
            "api",
            &snapshot,
            Err(ProviderTestFailure {
                status: Some(401),
                message: "provider returned 401 Unauthorized".to_string(),
                classification: None,
            }),
            Some(ProviderAccountInfo {
                usage_available: Some(88.0),
                last_refresh_at: Some("2026-08-06T09:00:00Z".to_string()),
                ..ProviderAccountInfo::default()
            }),
            None,
        )
        .expect("persist refresh");

        assert_eq!(health.status, HealthStatusKind::AuthFailed);
        let config = store.load().expect("config");
        assert_eq!(
            config.providers["api"]
                .account
                .as_ref()
                .and_then(|account| account.usage_available),
            Some(88.0)
        );
    }
}
