use crate::{ProviderConfig, ProviderKind};
use serde_json::Value;

pub const COMPANION_OFFICIAL_AUTH_MODE_FIELD: &str = "codex_companion_auth_mode";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficialAuthMode {
    OAuth,
    Pat,
    AgentIdentity,
}

impl OfficialAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::Pat => "pat",
            Self::AgentIdentity => "agentIdentity",
        }
    }
}

/// The credential Relay can inject. Official providers deliberately use the
/// same canonical source for Relay, direct launch, and export.
pub fn provider_relay_auth_ref(provider: &ProviderConfig) -> Option<&str> {
    non_empty(provider.auth_ref.as_deref())
        .or_else(|| non_empty(provider.direct_auth_ref.as_deref()))
}

/// The credential a direct launch can use. Third-party providers may keep an
/// environment variable exclusively for direct launch, while official
/// providers always follow their canonical account credential.
pub fn provider_direct_auth_ref(provider: &ProviderConfig) -> Option<&str> {
    if provider.kind == ProviderKind::OfficialCodex {
        return provider_relay_auth_ref(provider);
    }
    non_empty(provider.direct_auth_ref.as_deref())
        .or_else(|| non_empty(provider.auth_ref.as_deref()))
}

pub fn official_auth_mode_from_account(provider: &ProviderConfig) -> Option<OfficialAuthMode> {
    provider
        .account
        .as_ref()
        .and_then(|account| account.auth_mode.as_deref())
        .and_then(parse_official_auth_mode)
}

pub fn parse_official_auth_mode(value: &str) -> Option<OfficialAuthMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "oauth" | "chatgpt" | "chat_gpt" | "openai_oauth" => Some(OfficialAuthMode::OAuth),
        "pat"
        | "personal_access_token"
        | "personalaccesstoken"
        | "token"
        | "apikey"
        | "api_key" => Some(OfficialAuthMode::Pat),
        "agentidentity" | "agent_identity" => Some(OfficialAuthMode::AgentIdentity),
        _ => None,
    }
}

/// Detects the explicit authentication material in a stored auth JSON value.
/// A Companion marker has priority because Codex itself stores both OAuth and
/// PAT sessions with `auth_mode: "chatgpt"`.
pub fn official_auth_mode_from_auth_json(value: &Value) -> Option<OfficialAuthMode> {
    let sources = auth_sources(value);
    for source in &sources {
        if let Some(mode) = source
            .get(COMPANION_OFFICIAL_AUTH_MODE_FIELD)
            .and_then(Value::as_str)
            .and_then(parse_official_auth_mode)
        {
            return Some(mode);
        }
    }

    if sources.iter().any(|source| {
        source
            .get("auth_mode")
            .or_else(|| source.get("authMode"))
            .or_else(|| source.get("openai_auth_mode"))
            .and_then(Value::as_str)
            .and_then(parse_official_auth_mode)
            .is_some_and(|mode| mode == OfficialAuthMode::AgentIdentity)
            || (non_empty_json_string(source, "agent_runtime_id").is_some()
                || non_empty_json_string(source, "agentRuntimeId").is_some())
                && (non_empty_json_string(source, "agent_private_key").is_some()
                    || non_empty_json_string(source, "agentPrivateKey").is_some())
    }) {
        return Some(OfficialAuthMode::AgentIdentity);
    }

    for source in &sources {
        if let Some(mode) = source
            .get("auth_mode")
            .or_else(|| source.get("authMode"))
            .or_else(|| source.get("openai_auth_mode"))
            .and_then(Value::as_str)
            .and_then(parse_auth_file_mode)
        {
            return Some(mode);
        }
    }

    if sources.iter().any(|source| {
        source
            .get("type")
            .and_then(Value::as_str)
            .and_then(parse_auth_file_mode)
            .is_some_and(|mode| mode == OfficialAuthMode::Pat)
    }) {
        return Some(OfficialAuthMode::Pat);
    }

    if sources.iter().any(|source| {
        ["personal_access_token", "personalAccessToken", "pat"]
            .into_iter()
            .any(|key| non_empty_json_string(source, key).is_some())
    }) {
        return Some(OfficialAuthMode::Pat);
    }

    if sources.iter().any(|source| {
        [
            "refresh_token",
            "refreshToken",
            "id_token",
            "idToken",
            "access_token",
            "accessToken",
        ]
        .into_iter()
        .any(|key| non_empty_json_string(source, key).is_some())
    }) {
        return Some(OfficialAuthMode::OAuth);
    }

    sources
        .iter()
        .any(|source| non_empty_json_string(source, "token").is_some())
        .then_some(OfficialAuthMode::Pat)
}

/// Resolves the bearer token stored in an official-account export without
/// confusing an explicit personal access token with an adjacent API-key field.
/// The auth-file marker takes precedence over stale provider metadata.
pub fn official_access_token_from_auth_json(
    value: &Value,
    fallback_mode: Option<OfficialAuthMode>,
) -> Option<String> {
    let sources = auth_sources(value);
    let mode = official_auth_mode_from_auth_json(value).or(fallback_mode);
    if mode == Some(OfficialAuthMode::AgentIdentity) {
        return None;
    }

    let access_token = pick_source_string(&sources, &["access_token", "accessToken"]);
    let personal_access_token = pick_source_string(
        &sources,
        &[
            "personal_access_token",
            "personalAccessToken",
            "pat",
            "token",
        ],
    );
    match mode {
        Some(OfficialAuthMode::Pat) => personal_access_token.or(access_token),
        Some(OfficialAuthMode::OAuth) | None => access_token.or(personal_access_token),
        Some(OfficialAuthMode::AgentIdentity) => None,
    }
}

fn parse_auth_file_mode(value: &str) -> Option<OfficialAuthMode> {
    let mode = parse_official_auth_mode(value)?;
    // Cockpit-style PAT exports have a dedicated personal token field. A bare
    // `auth_mode: api_key` is also common in ordinary API-key JSON and must
    // not route that credential into the official-account import path.
    if mode == OfficialAuthMode::Pat
        && matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "apikey" | "api_key"
        )
    {
        return None;
    }
    Some(mode)
}

fn auth_sources(value: &Value) -> Vec<&Value> {
    let account = value
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|accounts| {
            accounts.iter().find(|account| {
                account
                    .get("platform")
                    .and_then(Value::as_str)
                    .is_none_or(|platform| platform.eq_ignore_ascii_case("openai"))
            })
        });
    let primary = account.unwrap_or(value);
    let mut sources = vec![primary];
    for source in [
        primary.get("tokens"),
        primary.get("credentials"),
        primary.get("auth"),
        primary.get("extra"),
        value.get("tokens"),
        value.get("credentials"),
        value.get("auth"),
        value.get("extra"),
    ]
    .into_iter()
    .flatten()
    {
        if !sources
            .iter()
            .any(|existing| std::ptr::eq(*existing, source))
        {
            sources.push(source);
        }
    }
    if !sources
        .iter()
        .any(|existing| std::ptr::eq(*existing, value))
    {
        sources.push(value);
    }
    sources
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_json_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn pick_source_string(sources: &[&Value], keys: &[&str]) -> Option<String> {
    sources.iter().find_map(|source| {
        keys.iter()
            .find_map(|key| non_empty_json_string(source, key).map(ToOwned::to_owned))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{default_refresh_interval_seconds, ProviderKind};
    use std::collections::BTreeMap;

    fn provider(
        kind: ProviderKind,
        auth_ref: Option<&str>,
        direct_auth_ref: Option<&str>,
    ) -> ProviderConfig {
        ProviderConfig {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            kind,
            base_url: "https://example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: auth_ref.map(ToOwned::to_owned),
            direct_auth_ref: direct_auth_ref.map(ToOwned::to_owned),
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn chooses_auth_refs_by_connection_mode() {
        let third_party = provider(
            ProviderKind::OpenAiCompatible,
            Some("file:/relay.json"),
            Some("env:DIRECT_TOKEN"),
        );
        assert_eq!(
            provider_relay_auth_ref(&third_party),
            Some("file:/relay.json")
        );
        assert_eq!(
            provider_direct_auth_ref(&third_party),
            Some("env:DIRECT_TOKEN")
        );

        let official = provider(
            ProviderKind::OfficialCodex,
            Some("file:/official.json"),
            Some("file:/stale-direct.json"),
        );
        assert_eq!(
            provider_relay_auth_ref(&official),
            Some("file:/official.json")
        );
        assert_eq!(
            provider_direct_auth_ref(&official),
            Some("file:/official.json")
        );
    }

    #[test]
    fn companion_pat_marker_wins_over_codex_chatgpt_mode() {
        let value = serde_json::json!({
            "auth_mode": "chatgpt",
            "codex_companion_auth_mode": "pat",
            "tokens": { "access_token": "pat-token" }
        });

        assert_eq!(
            official_auth_mode_from_auth_json(&value),
            Some(OfficialAuthMode::Pat)
        );
    }

    #[test]
    fn detects_legacy_personal_access_token() {
        let value = serde_json::json!({ "personal_access_token": "pat-token" });

        assert_eq!(
            official_auth_mode_from_auth_json(&value),
            Some(OfficialAuthMode::Pat)
        );
    }

    #[test]
    fn bare_api_key_mode_is_not_mistaken_for_a_personal_access_token() {
        let value = serde_json::json!({
            "auth_mode": "api_key",
            "access_token": "unclassified-token"
        });

        assert_eq!(
            official_auth_mode_from_auth_json(&value),
            Some(OfficialAuthMode::OAuth)
        );
    }

    #[test]
    fn explicit_pat_wins_over_an_adjacent_api_key_or_access_token() {
        let value = serde_json::json!({
            "codex_companion_auth_mode": "pat",
            "api_key": "wrong-api-key",
            "access_token": "stale-access-token",
            "personal_access_token": "pat-token"
        });

        assert_eq!(
            official_access_token_from_auth_json(&value, None).as_deref(),
            Some("pat-token")
        );
    }

    #[test]
    fn oauth_prefers_access_token_over_a_nested_pat_field() {
        let value = serde_json::json!({
            "auth_mode": "oauth",
            "tokens": {
                "access_token": "oauth-token",
                "personal_access_token": "stale-pat-token"
            }
        });

        assert_eq!(
            official_access_token_from_auth_json(&value, None).as_deref(),
            Some("oauth-token")
        );
    }
}
