use codex_companion_core::{
    official_access_token_from_auth_json, official_auth_mode_from_account,
    official_auth_mode_from_auth_json, provider_relay_auth_ref, ProviderConfig, ProviderKind,
};
use std::fs;

pub fn resolve_auth_token(provider: &ProviderConfig) -> Option<String> {
    let auth_ref = provider_relay_auth_ref(provider)?;
    if let Some(name) = auth_ref.strip_prefix("env:") {
        return std::env::var(name).ok().filter(|value| !value.is_empty());
    }
    if let Some(path) = auth_ref.strip_prefix("file:") {
        let text = fs::read_to_string(path).ok()?;
        let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        if provider.kind == ProviderKind::OfficialCodex {
            return official_access_token_from_auth_json(
                &value,
                official_auth_mode_from_account(provider),
            );
        }
        return pick_string(
            &value,
            &[
                &["api_key"],
                &["OPENAI_API_KEY"],
                &["personal_access_token"],
                &["personalAccessToken"],
                &["pat"],
                &["access_token"],
                &["token"],
                &["credentials", "api_key"],
                &["credentials", "personal_access_token"],
                &["credentials", "personalAccessToken"],
                &["credentials", "pat"],
                &["tokens", "access_token"],
                &["tokens", "personal_access_token"],
                &["tokens", "personalAccessToken"],
                &["tokens", "pat"],
                &["credentials", "access_token"],
                &["tokens", "token"],
                &["credentials", "token"],
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
            let auth_ref = provider_relay_auth_ref(provider)?;
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

/// Keeps file-backed official account metadata aligned with its credential.
///
/// The auth file is the source of truth for an official account because it can
/// distinguish OAuth, PAT, and Agent Identity material. Environment-backed
/// accounts remain metadata-driven: Companion cannot inspect their secret and
/// must preserve the user's selected mode.
pub fn sync_official_auth_mode(provider: &mut ProviderConfig) -> bool {
    if provider.kind != ProviderKind::OfficialCodex {
        return false;
    }

    let mode = provider_relay_auth_ref(provider)
        .and_then(|auth_ref| auth_ref.strip_prefix("file:"))
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|auth| official_auth_mode_from_auth_json(&auth));
    let Some(mode) = mode else {
        return false;
    };
    if official_auth_mode_from_account(provider) == Some(mode) {
        return false;
    }

    let account = provider.account.get_or_insert_with(Default::default);
    account.auth_mode = Some(mode.as_str().to_string());
    true
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::default_refresh_interval_seconds;
    use std::collections::BTreeMap;

    fn provider(auth_ref: String, direct_auth_ref: String) -> ProviderConfig {
        ProviderConfig {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: Some(auth_ref),
            direct_auth_ref: Some(direct_auth_ref),
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn relay_token_resolution_prefers_relay_auth_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let relay_auth = temp.path().join("relay.json");
        let direct_auth = temp.path().join("direct.json");
        fs::write(&relay_auth, r#"{"api_key":"relay-token"}"#).expect("relay auth");
        fs::write(&direct_auth, r#"{"api_key":"direct-token"}"#).expect("direct auth");
        let provider = provider(
            format!("file:{}", relay_auth.display()),
            format!("file:{}", direct_auth.display()),
        );

        assert_eq!(
            resolve_auth_token(&provider).as_deref(),
            Some("relay-token")
        );
    }

    #[test]
    fn syncs_blank_agent_identity_auth_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("agent.json");
        fs::write(&auth_path, r#"{"auth_mode":"agentIdentity"}"#).expect("agent auth");
        let mut provider = ProviderConfig {
            id: "official".to_string(),
            name: "Official".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };

        assert!(sync_official_auth_mode(&mut provider));
        assert_eq!(
            provider
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
    }

    #[test]
    fn file_auth_mode_overrides_stale_official_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("oauth.json");
        fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"oauth-access","refresh_token":"oauth-refresh"}}"#,
        )
        .expect("oauth auth");
        let mut official = provider(format!("file:{}", auth_path.display()), String::new());
        official.kind = ProviderKind::OfficialCodex;
        official.account = Some(codex_companion_core::ProviderAccountInfo {
            auth_mode: Some("pat".to_string()),
            ..Default::default()
        });

        assert!(sync_official_auth_mode(&mut official));
        assert_eq!(
            official
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("oauth")
        );
    }

    #[test]
    fn environment_auth_preserves_official_metadata() {
        let mut official = provider("env:OFFICIAL_TOKEN".to_string(), String::new());
        official.kind = ProviderKind::OfficialCodex;
        official.account = Some(codex_companion_core::ProviderAccountInfo {
            auth_mode: Some("pat".to_string()),
            ..Default::default()
        });

        assert!(!sync_official_auth_mode(&mut official));
        assert_eq!(
            official
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("pat")
        );
    }

    #[test]
    fn official_pat_token_resolution_ignores_a_stale_api_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("pat.json");
        fs::write(
            &auth_path,
            r#"{
                "codex_companion_auth_mode":"pat",
                "api_key":"stale-api-key",
                "tokens":{"access_token":"stale-access-token","personal_access_token":"pat-token"}
            }"#,
        )
        .expect("pat auth");
        let mut official = provider(format!("file:{}", auth_path.display()), String::new());
        official.kind = ProviderKind::OfficialCodex;

        assert_eq!(resolve_auth_token(&official).as_deref(), Some("pat-token"));
    }
}
