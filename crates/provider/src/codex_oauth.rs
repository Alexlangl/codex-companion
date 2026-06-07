use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use codex_companion_core::{CompanionError, ProviderConfig, Result};
use serde::Deserialize;
use std::{fs, path::PathBuf};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const TOKEN_REFRESH_SKEW_SECONDS: i64 = 300;

#[derive(Debug, Clone)]
pub struct CodexAuthSnapshot {
    pub access_token: String,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug)]
struct CodexAuthFile {
    path: PathBuf,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
struct RawCodexAuthSnapshot {
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
    email: Option<String>,
    name: Option<String>,
    plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

pub fn load_codex_auth_snapshot(provider: &ProviderConfig) -> Result<CodexAuthSnapshot> {
    snapshot_with_access(read_codex_auth_file(provider)?.snapshot())
}

pub async fn ensure_codex_auth_snapshot(provider: &ProviderConfig) -> Result<CodexAuthSnapshot> {
    let mut auth_file = read_codex_auth_file(provider)?;
    let raw = auth_file.snapshot();
    let needs_refresh = raw
        .access_token
        .as_deref()
        .is_none_or(access_token_needs_refresh);

    if !needs_refresh {
        return snapshot_with_access(raw);
    }

    let refresh_token = raw.refresh_token.as_deref().ok_or_else(|| {
        CompanionError::InvalidConfig(
            "Codex 官方账号 access_token 已过期或缺失，且缺少 refresh_token".to_string(),
        )
    })?;
    let refreshed = refresh_tokens(refresh_token).await?;
    apply_refreshed_tokens(&mut auth_file.value, &refreshed);
    write_auth_file(&auth_file)?;
    snapshot_with_access(auth_file.snapshot())
}

async fn refresh_tokens(refresh_token: &str) -> Result<TokenRefreshResponse> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CODEX_OAUTH_CLIENT_ID),
    ];
    let response = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .await
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("刷新 Codex OAuth token 失败: {source}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|source| {
        CompanionError::InvalidConfig(format!("读取 Codex OAuth token 响应失败: {source}"))
    })?;
    if !status.is_success() {
        return Err(CompanionError::InvalidConfig(format!(
            "Codex OAuth token 刷新接口返回 {status} [body_len:{}]",
            body.len()
        )));
    }
    serde_json::from_str::<TokenRefreshResponse>(&body).map_err(|source| {
        CompanionError::InvalidConfig(format!("解析 Codex OAuth token 响应失败: {source}"))
    })
}

fn read_codex_auth_file(provider: &ProviderConfig) -> Result<CodexAuthFile> {
    let auth_ref = provider.auth_ref.as_deref().ok_or_else(|| {
        CompanionError::InvalidConfig(format!("provider {} 缺少 auth_ref", provider.id))
    })?;
    let path = auth_ref.strip_prefix("file:").ok_or_else(|| {
        CompanionError::InvalidConfig("Codex 官方账号需要 file: auth_ref".to_string())
    })?;
    let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|source| CompanionError::json(path, source))?;
    Ok(CodexAuthFile {
        path: PathBuf::from(path),
        value,
    })
}

fn write_auth_file(auth_file: &CodexAuthFile) -> Result<()> {
    let text = serde_json::to_string_pretty(&auth_file.value).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex auth 失败: {source}"))
    })?;
    let file_name = auth_file
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("auth.json");
    let tmp_path = auth_file.path.with_file_name(format!(".{file_name}.tmp"));
    fs::write(&tmp_path, text).map_err(|source| CompanionError::io(&tmp_path, source))?;
    fs::rename(&tmp_path, &auth_file.path)
        .map_err(|source| CompanionError::io(&auth_file.path, source))
}

impl CodexAuthFile {
    fn snapshot(&self) -> RawCodexAuthSnapshot {
        let tokens = self.value.get("tokens").unwrap_or(&serde_json::Value::Null);
        let sources = [tokens, &self.value];
        RawCodexAuthSnapshot {
            access_token: pick_first_string(
                &sources,
                &[
                    &["access_token"],
                    &["accessToken"],
                    &["credentials", "access_token"],
                    &["credentials", "accessToken"],
                ],
            ),
            refresh_token: pick_first_string(
                &sources,
                &[
                    &["refresh_token"],
                    &["refreshToken"],
                    &["credentials", "refresh_token"],
                    &["credentials", "refreshToken"],
                ],
            ),
            account_id: pick_first_string(
                &sources,
                &[
                    &["chatgpt_account_id"],
                    &["account_id"],
                    &["accountId"],
                    &["workspace_id"],
                    &["credentials", "chatgpt_account_id"],
                    &["credentials", "account_id"],
                ],
            ),
            email: pick_first_string(
                &sources,
                &[&["email"], &["name"], &["credentials", "email"]],
            ),
            name: pick_first_string(&sources, &[&["name"], &["display_name"], &["displayName"]]),
            plan_type: pick_first_string(
                &sources,
                &[
                    &["plan_type"],
                    &["planType"],
                    &["chatgpt_plan_type"],
                    &["credentials", "plan_type"],
                ],
            ),
        }
    }
}

fn snapshot_with_access(raw: RawCodexAuthSnapshot) -> Result<CodexAuthSnapshot> {
    Ok(CodexAuthSnapshot {
        access_token: raw.access_token.ok_or_else(|| {
            CompanionError::InvalidConfig("Codex auth 缺少 access_token".to_string())
        })?,
        account_id: raw.account_id,
        email: raw.email,
        name: raw.name,
        plan_type: raw.plan_type,
    })
}

fn apply_refreshed_tokens(value: &mut serde_json::Value, refreshed: &TokenRefreshResponse) {
    ensure_object(value);
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let tokens = root
        .entry("tokens")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    ensure_object(tokens);
    let Some(tokens) = tokens.as_object_mut() else {
        return;
    };
    tokens.insert(
        "access_token".to_string(),
        serde_json::Value::String(refreshed.access_token.clone()),
    );
    if let Some(id_token) = refreshed
        .id_token
        .as_ref()
        .and_then(|value| normalize_optional(value))
    {
        tokens.insert("id_token".to_string(), serde_json::Value::String(id_token));
    }
    if let Some(refresh_token) = refreshed
        .refresh_token
        .as_ref()
        .and_then(|value| normalize_optional(value))
    {
        tokens.insert(
            "refresh_token".to_string(),
            serde_json::Value::String(refresh_token),
        );
    }
    let now = Utc::now().to_rfc3339();
    tokens.insert(
        "last_refresh".to_string(),
        serde_json::Value::String(now.clone()),
    );
    tokens.insert("expired".to_string(), serde_json::Value::Bool(false));
    let _ = tokens;
    root.insert("last_refresh".to_string(), serde_json::Value::String(now));
    root.insert("expired".to_string(), serde_json::Value::Bool(false));
}

fn ensure_object(value: &mut serde_json::Value) {
    if !value.is_object() {
        *value = serde_json::Value::Object(serde_json::Map::new());
    }
}

fn access_token_needs_refresh(token: &str) -> bool {
    let now = Utc::now().timestamp();
    access_token_needs_refresh_at(token, now)
}

fn access_token_needs_refresh_at(token: &str, now: i64) -> bool {
    jwt_exp(token)
        .map(|expires_at| expires_at <= now + TOKEN_REFRESH_SKEW_SECONDS)
        .unwrap_or(false)
}

fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .or_else(|_| URL_SAFE.decode(payload.as_bytes()))
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    value.get("exp").and_then(serde_json::Value::as_i64)
}

fn pick_first_string(value: &[&serde_json::Value], paths: &[&[&str]]) -> Option<String> {
    for source in value {
        for path in paths {
            if let Some(text) = get_path(source, path)
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_optional)
            {
                return Some(text);
            }
        }
    }
    None
}

fn get_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with_exp(exp: i64) -> String {
        let payload = serde_json::json!({ "exp": exp }).to_string();
        let encoded = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("header.{encoded}.signature")
    }

    #[test]
    fn detects_expiring_access_token() {
        let token = jwt_with_exp(1_000);
        assert!(access_token_needs_refresh_at(&token, 800));
        assert!(!access_token_needs_refresh_at(&token, 100));
    }

    #[test]
    fn leaves_opaque_tokens_usable() {
        assert!(!access_token_needs_refresh_at("not-a-jwt", 1_000));
    }

    #[test]
    fn applies_refreshed_tokens_without_losing_refresh_token() {
        let mut value = serde_json::json!({
            "tokens": {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "id_token": "old-id"
            }
        });
        apply_refreshed_tokens(
            &mut value,
            &TokenRefreshResponse {
                access_token: "new-access".to_string(),
                id_token: None,
                refresh_token: None,
            },
        );
        let tokens = value.get("tokens").expect("tokens");
        assert_eq!(tokens["access_token"], "new-access");
        assert_eq!(tokens["refresh_token"], "old-refresh");
        assert_eq!(tokens["id_token"], "old-id");
        assert_eq!(tokens["expired"], false);
    }
}
