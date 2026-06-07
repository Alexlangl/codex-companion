use crate::account_refresh::{refresh_api_key_usage, refresh_official_codex_account};
use crate::auth::resolve_auth_token;
use chrono::Utc;
use codex_companion_core::{
    CompanionError, ConfigStore, ProviderConfig, ProviderHealth, ProviderKind, Result,
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
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
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
    let account_result = if provider.kind == ProviderKind::OfficialCodex {
        Some(refresh_official_codex_account(&provider).await)
    } else {
        None
    };
    let result = match account_result.as_ref() {
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(error.to_string()),
        None => test_provider(&provider).await,
    };
    let api_usage_result = if result.is_ok() && provider.kind != ProviderKind::OfficialCodex {
        refresh_api_key_usage(&provider).await.ok()
    } else {
        None
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
                    } else if let Some(account) = api_usage_result {
                        provider.account = Some(account);
                    } else if let Some(account) = provider.account.as_mut() {
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
