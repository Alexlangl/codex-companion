use crate::account_refresh::{
    provider_supports_api_key_usage, refresh_api_key_usage, refresh_official_codex_account,
};
use crate::auth::resolve_auth_token;
use chrono::Utc;
use codex_companion_core::{
    provider_api_base_url, CompanionError, ConfigStore, ProviderConfig, ProviderHealth,
    ProviderKind, Result,
};
use codex_companion_health::{classify_failure, mark_failure, mark_success};

pub async fn test_provider(provider: &ProviderConfig) -> std::result::Result<(), String> {
    if provider.kind == ProviderKind::OfficialCodex {
        return refresh_official_codex_account(provider)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
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
        .map_err(|error| format!("network failed: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(format!("provider returned {status}: {body}"))
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
        let health_result = result.as_ref().map(|_| ()).map_err(ToString::to_string);
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
                test_provider(&provider).await
            }
        }
    } else {
        test_provider(&provider).await
    };
    store.update(|config| {
        let now = Utc::now();
        let health = config.health.entry(id.to_string()).or_default();
        match result {
            Ok(()) => {
                mark_success(health);
                if let Some(provider) = config.providers.get_mut(id) {
                    if let Some(Ok(account)) = account_result {
                        provider.account = Some(account);
                    } else if let Some(error) = api_usage_error {
                        let mut account = provider.account.clone().unwrap_or_default();
                        apply_usage_refresh_failure(&mut account, &provider.name, &error, now);
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
            Err(message) => {
                let failure = classify_failure(None, &message);
                mark_failure(health, &failure, message);
            }
        }
        Ok(health.clone())
    })
}

fn apply_usage_refresh_failure(
    account: &mut codex_companion_core::ProviderAccountInfo,
    provider_name: &str,
    error: &str,
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
        account.subscription_status = Some(format!("连接正常；额度刷新失败：{error}"));
    } else {
        account.subscription_status = Some("连接正常".to_string());
        clear_api_key_usage(account);
        account.quota_label = Some(error.to_string());
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
    use codex_companion_core::{ProviderAccountInfo, ProviderQuotaWindow};

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

        apply_usage_refresh_failure(
            &mut account,
            "Provider",
            "429 Too Many Requests",
            Utc::now(),
        );

        assert_eq!(account.usage_available, Some(75.0));
        assert_eq!(account.quota_percent, Some(75.0));
        assert_eq!(account.quota_windows.len(), 1);
        assert_eq!(account.quota_label.as_deref(), Some("本月剩余"));
        assert_eq!(
            account.last_refresh_at.as_deref(),
            Some(refreshed_at.as_str())
        );
        assert!(account
            .subscription_status
            .as_deref()
            .is_some_and(|status| status.contains("额度刷新失败")));
    }

    #[test]
    fn first_usage_failure_exposes_error_without_inventing_snapshot() {
        let mut account = ProviderAccountInfo::default();
        apply_usage_refresh_failure(&mut account, "Provider", "timeout", Utc::now());

        assert_eq!(account.quota_label.as_deref(), Some("timeout"));
        assert!(account.last_refresh_at.is_some());
        assert!(!has_usage_snapshot(&account));
    }
}
