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
                        account.display_name =
                            account.display_name.or_else(|| Some(provider.name.clone()));
                        account.subscription_type = account
                            .subscription_type
                            .or_else(|| Some("API Key".to_string()));
                        account.subscription_status = Some("连接正常".to_string());
                        clear_api_key_usage(&mut account);
                        account.quota_label = Some(error);
                        account.last_refresh_at = Some(now.to_rfc3339());
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

fn clear_api_key_usage(account: &mut codex_companion_core::ProviderAccountInfo) {
    account.quota_label = None;
    account.quota_percent = None;
    account.quota_reset_at = None;
    account.quota_windows.clear();
    account.usage_total = None;
    account.usage_used = None;
    account.usage_available = None;
}
