use codex_companion_core::ProviderConfig;
use std::fs;

pub fn resolve_auth_token(provider: &ProviderConfig) -> Option<String> {
    let auth_ref = provider.auth_ref.as_ref()?;
    if let Some(name) = auth_ref.strip_prefix("env:") {
        return std::env::var(name).ok().filter(|value| !value.is_empty());
    }
    if let Some(path) = auth_ref.strip_prefix("file:") {
        let text = fs::read_to_string(path).ok()?;
        let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        return pick_string(
            &value,
            &[
                &["api_key"],
                &["OPENAI_API_KEY"],
                &["access_token"],
                &["token"],
                &["credentials", "api_key"],
                &["tokens", "access_token"],
                &["credentials", "access_token"],
            ],
        );
    }
    None
}

pub fn resolve_chatgpt_account_id(provider: &ProviderConfig) -> Option<String> {
    provider
        .account
        .as_ref()
        .and_then(|account| account.account_id.clone())
        .or_else(|| {
            let auth_ref = provider.auth_ref.as_ref()?;
            let path = auth_ref.strip_prefix("file:")?;
            let text = fs::read_to_string(path).ok()?;
            let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
            pick_string(
                &value,
                &[
                    &["chatgpt_account_id"],
                    &["account_id"],
                    &["tokens", "chatgpt_account_id"],
                    &["tokens", "account_id"],
                    &["credentials", "chatgpt_account_id"],
                    &["credentials", "account_id"],
                ],
            )
        })
}

fn pick_string(value: &serde_json::Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut cursor = value;
        let mut found = true;
        for key in *path {
            match cursor.get(*key) {
                Some(next) => cursor = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            if let Some(text) = cursor
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(text.to_string());
            }
        }
    }
    None
}
