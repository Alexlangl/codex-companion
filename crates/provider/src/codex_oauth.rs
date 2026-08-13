use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use chrono::Utc;
use codex_companion_core::{
    http_client_builder, official_auth_mode_from_account, official_auth_mode_from_auth_json,
    provider_relay_auth_ref, redact_sensitive_text, CompanionError, HealthFailureKind,
    OfficialAuthMode, ProviderConfig, ProviderKind, Result,
};
use codex_companion_health::{classification_for_kind, FailureClassification};
use serde::Deserialize;
use std::fmt;
use std::{
    fs,
    path::{Path, PathBuf},
};

const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const TOKEN_REFRESH_SKEW_SECONDS: i64 = 300;
const OPAQUE_TOKEN_REFRESH_INTERVAL_SECONDS: i64 = 30 * 60;
const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;
const TOKEN_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const TOKEN_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const AUTH_FILE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);
const AUTH_FILE_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct CodexAuthSnapshot {
    pub access_token: String,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexOAuthError {
    pub kind: HealthFailureKind,
    pub status: Option<u16>,
    pub message: String,
}

impl CodexOAuthError {
    fn new(kind: HealthFailureKind, status: Option<u16>, message: impl fmt::Display) -> Self {
        Self {
            kind,
            status,
            message: message.to_string(),
        }
    }

    fn auth(message: impl fmt::Display) -> Self {
        Self::new(HealthFailureKind::AuthFailed, None, message)
    }

    fn upstream(message: impl fmt::Display) -> Self {
        Self::new(HealthFailureKind::UpstreamFailed, None, message)
    }

    fn network(message: impl fmt::Display) -> Self {
        Self::new(HealthFailureKind::NetworkFailed, None, message)
    }

    pub fn failure_classification(&self) -> FailureClassification {
        classification_for_kind(self.kind.clone())
    }

    pub fn into_companion_error(self) -> CompanionError {
        CompanionError::InvalidConfig(self.message)
    }
}

impl fmt::Display for CodexOAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for CodexOAuthError {}

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
    expires_at: Option<i64>,
    expired: Option<bool>,
    last_refresh_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

pub fn load_codex_auth_snapshot(provider: &ProviderConfig) -> Result<CodexAuthSnapshot> {
    snapshot_with_access(read_codex_auth_file(provider)?.snapshot())
}

pub async fn ensure_codex_auth_snapshot(provider: &ProviderConfig) -> Result<CodexAuthSnapshot> {
    ensure_codex_auth_snapshot_detailed(provider)
        .await
        .map_err(CodexOAuthError::into_companion_error)
}

pub async fn ensure_codex_auth_snapshot_detailed(
    provider: &ProviderConfig,
) -> std::result::Result<CodexAuthSnapshot, CodexOAuthError> {
    Ok(ensure_codex_auth_snapshot_with_status_detailed(provider)
        .await?
        .0)
}

/// Returns the current OAuth snapshot and whether this call observed a token
/// rotation before the request was sent. Callers still retry one upstream 401:
/// the refresh helper reuses a token rotated by another process when possible.
pub async fn ensure_codex_auth_snapshot_with_status(
    provider: &ProviderConfig,
) -> Result<(CodexAuthSnapshot, bool)> {
    ensure_codex_auth_snapshot_with_status_detailed(provider)
        .await
        .map_err(CodexOAuthError::into_companion_error)
}

pub async fn ensure_codex_auth_snapshot_with_status_detailed(
    provider: &ProviderConfig,
) -> std::result::Result<(CodexAuthSnapshot, bool), CodexOAuthError> {
    refresh_codex_auth_snapshot_detailed(provider, None).await
}

/// Refreshes the official OAuth credentials after the upstream rejected the
/// access token. `failed_access_token` is used to avoid refreshing twice when
/// another Companion process has already rotated the token while this request
/// was in flight.
pub async fn refresh_codex_auth_snapshot_after_unauthorized(
    provider: &ProviderConfig,
    failed_access_token: &str,
) -> Result<CodexAuthSnapshot> {
    refresh_codex_auth_snapshot_after_unauthorized_detailed(provider, failed_access_token)
        .await
        .map_err(CodexOAuthError::into_companion_error)
}

pub async fn refresh_codex_auth_snapshot_after_unauthorized_detailed(
    provider: &ProviderConfig,
    failed_access_token: &str,
) -> std::result::Result<CodexAuthSnapshot, CodexOAuthError> {
    Ok(
        refresh_codex_auth_snapshot_detailed(provider, Some(failed_access_token))
            .await?
            .0,
    )
}

async fn refresh_codex_auth_snapshot_detailed(
    provider: &ProviderConfig,
    failed_access_token: Option<&str>,
) -> std::result::Result<(CodexAuthSnapshot, bool), CodexOAuthError> {
    let auth_file = read_codex_auth_file(provider).map_err(CodexOAuthError::auth)?;
    let raw = auth_file.snapshot();
    let initial_access_token = raw.access_token.clone();
    let needs_refresh = failed_access_token.is_some()
        || raw.access_token.is_none()
        || auth_snapshot_needs_refresh(&raw, Utc::now().timestamp());

    if !needs_refresh {
        return Ok((
            snapshot_with_access(raw).map_err(CodexOAuthError::auth)?,
            false,
        ));
    }

    // 刷新必须跨进程串行：desktop 与 daemon(或 live-follow 的别的进程)可能
    // 同时刷同一个 auth 文件，各自拿同一个 refresh_token 去刷会触发
    // invalid_grant，乱序写回还可能把已作废的 refresh_token 持久化。进程内
    // 的 Mutex 挡不住别的进程，这里对 auth 文件旁的 .lock 哨兵文件加独占
    // flock；拿锁后重读文件做双重检查。
    let _guard = lock_auth_file(&auth_file.path)
        .await
        .map_err(CodexOAuthError::upstream)?;
    let mut auth_file = read_codex_auth_file(provider).map_err(CodexOAuthError::auth)?;
    let raw = auth_file.snapshot();
    if let Some(failed_access_token) = failed_access_token {
        // A different process may have refreshed the same auth file after the
        // request was sent. Reuse that newer token instead of rotating the
        // refresh token a second time.
        if raw
            .access_token
            .as_deref()
            .is_some_and(|token| token != failed_access_token)
        {
            return Ok((
                snapshot_with_access(raw).map_err(CodexOAuthError::auth)?,
                true,
            ));
        }
    } else if raw.access_token.is_some()
        && !auth_snapshot_needs_refresh(&raw, Utc::now().timestamp())
    {
        let rotated = initial_access_token.as_deref() != raw.access_token.as_deref();
        return Ok((
            snapshot_with_access(raw).map_err(CodexOAuthError::auth)?,
            rotated,
        ));
    }

    let refresh_token = raw.refresh_token.as_deref().ok_or_else(|| {
        CodexOAuthError::auth("Codex 官方账号 access_token 已过期或缺失，且缺少 refresh_token")
    })?;
    let refreshed = refresh_tokens(refresh_token).await?;
    apply_refreshed_tokens(&mut auth_file.value, &refreshed);
    write_auth_file(&auth_file).map_err(CodexOAuthError::upstream)?;
    Ok((
        snapshot_with_access(auth_file.snapshot()).map_err(CodexOAuthError::auth)?,
        true,
    ))
}

async fn refresh_tokens(
    refresh_token: &str,
) -> std::result::Result<TokenRefreshResponse, CodexOAuthError> {
    refresh_tokens_at(refresh_token, TOKEN_ENDPOINT).await
}

async fn refresh_tokens_at(
    refresh_token: &str,
    token_endpoint: &str,
) -> std::result::Result<TokenRefreshResponse, CodexOAuthError> {
    let client = http_client_builder()
        .timeout(TOKEN_REFRESH_TIMEOUT)
        .connect_timeout(TOKEN_CONNECT_TIMEOUT)
        .build()
        .map_err(|source| {
            CodexOAuthError::upstream(format!("创建 Codex OAuth 客户端失败: {source}"))
        })?;
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CODEX_OAUTH_CLIENT_ID),
    ];
    let response = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|source| {
            CodexOAuthError::network(format!("刷新 Codex OAuth token 失败: {source}"))
        })?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
    {
        return Err(CodexOAuthError::upstream("Codex OAuth token 响应过大"));
    }
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|source| {
        CodexOAuthError::network(format!("读取 Codex OAuth token 响应失败: {source}"))
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_TOKEN_RESPONSE_BYTES {
            return Err(CodexOAuthError::upstream("Codex OAuth token 响应过大"));
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(oauth_token_endpoint_error(status, &body));
    }
    let mut tokens = serde_json::from_slice::<TokenRefreshResponse>(&body).map_err(|source| {
        CodexOAuthError::upstream(format!("解析 Codex OAuth token 响应失败: {source}"))
    })?;
    let access_token = normalize_optional(&tokens.access_token)
        .ok_or_else(|| CodexOAuthError::upstream("Codex OAuth token 响应缺少 access_token"))?;
    tokens.access_token = access_token;
    Ok(tokens)
}

fn oauth_token_endpoint_error(status: reqwest::StatusCode, body: &[u8]) -> CodexOAuthError {
    let kind = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        HealthFailureKind::RateLimited
    } else if status.is_client_error() {
        HealthFailureKind::AuthFailed
    } else {
        HealthFailureKind::UpstreamFailed
    };
    let detail = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            [
                value.get("error").and_then(serde_json::Value::as_str),
                value
                    .get("error_description")
                    .and_then(serde_json::Value::as_str),
                value
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str),
            ]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|value| !value.is_empty())
            .map(|value| {
                redact_sensitive_text(value)
                    .chars()
                    .take(160)
                    .collect::<String>()
            })
        });
    let suffix = detail.map(|value| format!(": {value}")).unwrap_or_default();
    CodexOAuthError::new(
        kind,
        Some(status.as_u16()),
        format!(
            "Codex OAuth token 刷新接口返回 {status} [body_len:{}]{suffix}",
            body.len()
        ),
    )
}

/// 对 auth 文件旁的哨兵文件(`.auth.json.lock`)加有界等待的独占文件锁。锁按
/// auth 文件路径隔离；guard(打开的文件)释放时锁自动解除。锁文件必须与 auth
/// 文件分离：写回是 tmp+rename，rename 会替换 inode，直接锁 auth 文件本身会失效。
async fn lock_auth_file(auth_path: &Path) -> Result<fs::File> {
    lock_auth_file_with_timeout(auth_path, AUTH_FILE_LOCK_TIMEOUT).await
}

async fn lock_auth_file_with_timeout(
    auth_path: &Path,
    timeout: std::time::Duration,
) -> Result<fs::File> {
    let auth_path = auth_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file_name = auth_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("auth.json");
        let lock_path = auth_path.with_file_name(format!(".{file_name}.lock"));
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|source| CompanionError::io(&lock_path, source))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match lock_file.try_lock() {
                Ok(()) => return Ok(lock_file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(CompanionError::InvalidConfig(format!(
                            "等待 auth 文件锁超时: {}",
                            lock_path.display()
                        )));
                    }
                    std::thread::sleep(
                        AUTH_FILE_LOCK_RETRY_DELAY
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                }
                Err(std::fs::TryLockError::Error(source)) => {
                    return Err(CompanionError::io(&lock_path, source));
                }
            }
        }
    })
    .await
    .map_err(|source| CompanionError::InvalidConfig(format!("获取 auth 文件锁失败: {source}")))?
}

fn read_codex_auth_file(provider: &ProviderConfig) -> Result<CodexAuthFile> {
    let auth_ref = provider_relay_auth_ref(provider)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
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

/// Official Codex providers historically did not persist an auth mode, so an
/// official provider is treated as OAuth unless it is explicitly marked as a
/// PAT. This keeps old OAuth providers on the refresh-capable relay path while
/// allowing newly imported personal access tokens to remain direct-connectable.
pub fn provider_uses_codex_oauth(provider: &ProviderConfig) -> bool {
    if provider.kind != ProviderKind::OfficialCodex {
        return false;
    }
    if let Ok(auth_file) = read_codex_auth_file(provider) {
        if let Some(mode) = official_auth_mode_from_auth_json(&auth_file.value) {
            return mode == OfficialAuthMode::OAuth;
        }
    }
    official_auth_mode_from_account(provider)
        .map(|mode| mode == OfficialAuthMode::OAuth)
        // An unreadable legacy official auth file must not silently become a
        // direct provider. OAuth is the legacy-compatible default.
        .unwrap_or(true)
}

fn write_auth_file(auth_file: &CodexAuthFile) -> Result<()> {
    let text = serde_json::to_string_pretty(&auth_file.value).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex auth 失败: {source}"))
    })?;
    crate::write_private_auth_file(&auth_file.path, &format!("{text}\n"))
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
            expires_at: pick_first_timestamp(
                &sources,
                &[
                    &["expires_at"],
                    &["expiresAt"],
                    &["expired"],
                    &["credentials", "expires_at"],
                    &["credentials", "expiresAt"],
                    &["credentials", "expired"],
                ],
            ),
            expired: pick_first_bool(&sources, &[&["expired"], &["credentials", "expired"]]),
            last_refresh_at: pick_first_timestamp(
                &sources,
                &[
                    &["last_refresh"],
                    &["lastRefresh"],
                    &["last_refresh_at"],
                    &["lastRefreshAt"],
                    &["refreshed_at"],
                    &["refreshedAt"],
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
    let previous_expires_at = {
        let tokens = value.get("tokens").unwrap_or(&serde_json::Value::Null);
        let sources = [tokens, &*value];
        pick_first_timestamp(
            &sources,
            &[
                &["expires_at"],
                &["expiresAt"],
                &["expired"],
                &["credentials", "expires_at"],
                &["credentials", "expiresAt"],
                &["credentials", "expired"],
            ],
        )
    };
    ensure_object(value);
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let id_token = refreshed
        .id_token
        .as_ref()
        .and_then(|value| normalize_optional(value));
    let refresh_token = refreshed
        .refresh_token
        .as_ref()
        .and_then(|value| normalize_optional(value));
    let now = Utc::now();
    let expires_at = refreshed
        .expires_in
        .filter(|value| *value > 0)
        .map(|expires_in| now.timestamp().saturating_add(expires_in))
        .or_else(|| {
            previous_expires_at.filter(|expires_at| {
                *expires_at > now.timestamp().saturating_add(TOKEN_REFRESH_SKEW_SECONDS)
            })
        });
    let now_text = now.to_rfc3339();
    {
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
        if let Some(id_token) = id_token.as_ref() {
            tokens.insert(
                "id_token".to_string(),
                serde_json::Value::String(id_token.clone()),
            );
        }
        if let Some(refresh_token) = refresh_token.as_ref() {
            tokens.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token.clone()),
            );
        }
        if let Some(expires_at) = expires_at {
            tokens.insert(
                "expires_at".to_string(),
                serde_json::Value::Number(expires_at.into()),
            );
        } else {
            tokens.remove("expires_at");
            tokens.remove("expiresAt");
        }
        tokens.insert(
            "last_refresh".to_string(),
            serde_json::Value::String(now_text.clone()),
        );
        tokens.insert("expired".to_string(), serde_json::Value::Bool(false));
    }
    sync_refreshed_root_oauth_fields(
        root,
        &refreshed.access_token,
        id_token.as_deref(),
        refresh_token.as_deref(),
    );
    if let Some(expires_at) = expires_at {
        root.insert(
            "expires_at".to_string(),
            serde_json::Value::Number(expires_at.into()),
        );
    } else {
        root.remove("expires_at");
        root.remove("expiresAt");
    }
    root.insert(
        "last_refresh".to_string(),
        serde_json::Value::String(now_text),
    );
    root.insert("expired".to_string(), serde_json::Value::Bool(false));
}

fn sync_refreshed_root_oauth_fields(
    root: &mut serde_json::Map<String, serde_json::Value>,
    access_token: &str,
    id_token: Option<&str>,
    refresh_token: Option<&str>,
) {
    replace_existing_root_oauth_fields(root, &["access_token", "accessToken"], access_token);
    if let Some(id_token) = id_token {
        replace_existing_root_oauth_fields(root, &["id_token", "idToken"], id_token);
    }
    if let Some(refresh_token) = refresh_token {
        replace_existing_root_oauth_fields(root, &["refresh_token", "refreshToken"], refresh_token);
    }
}

fn replace_existing_root_oauth_fields(
    root: &mut serde_json::Map<String, serde_json::Value>,
    field_names: &[&str],
    refreshed_value: &str,
) {
    for field_name in field_names {
        if root.contains_key(*field_name) {
            root.insert(
                (*field_name).to_string(),
                serde_json::Value::String(refreshed_value.to_string()),
            );
        }
    }
}

fn ensure_object(value: &mut serde_json::Value) {
    if !value.is_object() {
        *value = serde_json::Value::Object(serde_json::Map::new());
    }
}

fn access_token_needs_refresh_at(token: &str, now: i64) -> bool {
    jwt_exp(token)
        .map(|expires_at| expires_at <= now.saturating_add(TOKEN_REFRESH_SKEW_SECONDS))
        .unwrap_or(false)
}

fn auth_snapshot_needs_refresh(raw: &RawCodexAuthSnapshot, now: i64) -> bool {
    let Some(access_token) = raw.access_token.as_deref() else {
        return true;
    };
    if raw.expired == Some(true) {
        return true;
    }
    if access_token_needs_refresh_at(access_token, now) {
        return true;
    }
    if let Some(expires_at) = raw.expires_at {
        return expires_at <= now.saturating_add(TOKEN_REFRESH_SKEW_SECONDS);
    }
    raw.refresh_token.is_some()
        && raw.last_refresh_at.is_none_or(|last_refresh| {
            last_refresh.saturating_add(OPAQUE_TOKEN_REFRESH_INTERVAL_SECONDS) <= now
        })
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

fn pick_first_timestamp(value: &[&serde_json::Value], paths: &[&[&str]]) -> Option<i64> {
    for source in value {
        for path in paths {
            if let Some(timestamp) = get_path(source, path).and_then(parse_timestamp) {
                return Some(timestamp);
            }
        }
    }
    None
}

fn pick_first_bool(value: &[&serde_json::Value], paths: &[&[&str]]) -> Option<bool> {
    for source in value {
        for path in paths {
            if let Some(value) = get_path(source, path).and_then(serde_json::Value::as_bool) {
                return Some(value);
            }
        }
    }
    None
}

fn parse_timestamp(value: &serde_json::Value) -> Option<i64> {
    if let Some(timestamp) = value.as_i64() {
        return Some(normalize_epoch_timestamp(timestamp));
    }
    let text = value.as_str()?.trim();
    text.parse::<i64>()
        .ok()
        .map(normalize_epoch_timestamp)
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|value| value.timestamp())
        })
}

fn normalize_epoch_timestamp(timestamp: i64) -> i64 {
    if timestamp >= 100_000_000_000 || timestamp <= -100_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    }
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
    use codex_companion_core::{
        default_refresh_interval_seconds, official_access_token_from_auth_json, ProviderKind,
    };
    use std::collections::BTreeMap;

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
    fn normalizes_second_and_millisecond_timestamps() {
        assert_eq!(
            parse_timestamp(&serde_json::json!(1_900_000_000)),
            Some(1_900_000_000)
        );
        assert_eq!(
            parse_timestamp(&serde_json::json!(1_900_000_000_000_i64)),
            Some(1_900_000_000)
        );
        assert_eq!(
            parse_timestamp(&serde_json::json!("2030-01-02T03:04:05Z")),
            Some(
                chrono::DateTime::parse_from_rfc3339("2030-01-02T03:04:05Z")
                    .expect("timestamp")
                    .timestamp(),
            )
        );
    }

    #[test]
    fn expired_boolean_forces_refresh() {
        let raw = RawCodexAuthSnapshot {
            access_token: Some("opaque-access".to_string()),
            refresh_token: Some("refresh".to_string()),
            expired: Some(true),
            ..Default::default()
        };

        assert!(auth_snapshot_needs_refresh(&raw, 1_000));
    }

    #[test]
    fn distinguishes_imported_pat_from_legacy_oauth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pat_path = temp.path().join("pat.json");
        fs::write(
            &pat_path,
            r#"{"auth_mode":"pat","tokens":{"access_token":"pat-token"}}"#,
        )
        .expect("pat auth");
        let pat = ProviderConfig {
            id: "pat".to_string(),
            name: "PAT".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", pat_path.display())),
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        assert!(!provider_uses_codex_oauth(&pat));

        let oauth_path = temp.path().join("oauth.json");
        fs::write(
            &oauth_path,
            r#"{"tokens":{"access_token":"oauth-token","refresh_token":"refresh-token"}}"#,
        )
        .expect("oauth auth");
        let mut oauth = pat;
        oauth.id = "oauth".to_string();
        oauth.auth_ref = None;
        oauth.direct_auth_ref = Some(format!("file:{}", oauth_path.display()));
        assert!(provider_uses_codex_oauth(&oauth));
    }

    #[test]
    fn proactively_refreshes_opaque_tokens_from_last_refresh_time() {
        let raw = RawCodexAuthSnapshot {
            access_token: Some("opaque-access".to_string()),
            refresh_token: Some("refresh".to_string()),
            last_refresh_at: Some(1_000),
            ..Default::default()
        };

        assert!(!auth_snapshot_needs_refresh(&raw, 1_000 + 1_799));
        assert!(auth_snapshot_needs_refresh(
            &raw,
            1_000 + OPAQUE_TOKEN_REFRESH_INTERVAL_SECONDS
        ));
    }

    #[test]
    fn refreshes_an_opaque_oauth_token_when_no_refresh_timestamp_exists() {
        let raw = RawCodexAuthSnapshot {
            access_token: Some("opaque-access".to_string()),
            refresh_token: Some("refresh".to_string()),
            ..Default::default()
        };

        assert!(auth_snapshot_needs_refresh(&raw, 1_000));
    }

    #[tokio::test]
    async fn auth_file_lock_blocks_second_acquirer_until_released() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        let first = lock_auth_file(&auth_path).await.expect("first lock");

        let acquired = Arc::new(AtomicBool::new(false));
        let observer = acquired.clone();
        let path = auth_path.clone();
        let second = tokio::spawn(async move {
            let _guard = lock_auth_file(&path).await.expect("second lock");
            observer.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !acquired.load(Ordering::SeqCst),
            "第一个 guard 未释放前，第二个获取者必须阻塞"
        );

        drop(first);
        second.await.expect("join second acquirer");
        assert!(acquired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn auth_file_lock_wait_is_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        let _first = lock_auth_file(&auth_path).await.expect("first lock");

        let error = lock_auth_file_with_timeout(&auth_path, std::time::Duration::from_millis(80))
            .await
            .expect_err("second lock should time out while first lock is held");

        assert!(error.to_string().contains("等待 auth 文件锁超时"));
    }

    #[tokio::test]
    async fn reuses_a_token_rotated_by_another_process_after_unauthorized() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("auth.json");
        fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"new-access","refresh_token":"new-refresh"}}"#,
        )
        .expect("auth");
        let provider = ProviderConfig {
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

        let snapshot = refresh_codex_auth_snapshot_after_unauthorized(&provider, "old-access")
            .await
            .expect("new token should be reused");

        assert_eq!(snapshot.access_token, "new-access");
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
                expires_in: Some(3_600),
            },
        );
        let tokens = value.get("tokens").expect("tokens");
        assert_eq!(tokens["access_token"], "new-access");
        assert_eq!(tokens["refresh_token"], "old-refresh");
        assert_eq!(tokens["id_token"], "old-id");
        assert_eq!(tokens["expired"], false);
        assert!(tokens["expires_at"].as_i64().is_some());
    }

    #[test]
    fn refresh_keeps_legacy_root_tokens_in_sync_with_nested_tokens() {
        let mut value = serde_json::json!({
            "auth_mode": "oauth",
            "access_token": "stale-root-access",
            "accessToken": "stale-root-access-camel",
            "refresh_token": "stale-root-refresh",
            "idToken": "stale-root-id",
            "tokens": {
                "access_token": "old-nested-access",
                "refresh_token": "old-nested-refresh"
            }
        });

        apply_refreshed_tokens(
            &mut value,
            &TokenRefreshResponse {
                access_token: "new-access".to_string(),
                id_token: Some("new-id".to_string()),
                refresh_token: Some("new-refresh".to_string()),
                expires_in: Some(3_600),
            },
        );

        assert_eq!(value["access_token"], "new-access");
        assert_eq!(value["accessToken"], "new-access");
        assert_eq!(value["refresh_token"], "new-refresh");
        assert_eq!(value["idToken"], "new-id");
        assert_eq!(value["tokens"]["access_token"], "new-access");
        assert_eq!(
            official_access_token_from_auth_json(&value, Some(OfficialAuthMode::OAuth)).as_deref(),
            Some("new-access")
        );
    }

    #[test]
    fn keeps_a_future_expiry_when_refresh_response_omits_expires_in() {
        let future = Utc::now().timestamp() + TOKEN_REFRESH_SKEW_SECONDS + 600;
        let mut value = serde_json::json!({
            "tokens": {
                "access_token": "old-access",
                "expires_at": future
            }
        });
        apply_refreshed_tokens(
            &mut value,
            &TokenRefreshResponse {
                access_token: "new-access".to_string(),
                id_token: None,
                refresh_token: None,
                expires_in: None,
            },
        );

        assert_eq!(value["tokens"]["expires_at"].as_i64(), Some(future));
        assert_eq!(value["expires_at"].as_i64(), Some(future));
    }

    #[tokio::test]
    async fn token_endpoint_failures_keep_their_health_classification() {
        use axum::{extract::Path, http::StatusCode, routing::post, Router};

        let app = Router::new().route(
            "/{status}",
            post(|Path(status): Path<u16>| async move {
                let status = StatusCode::from_u16(status).expect("valid status");
                (status, r#"{"error":"token refresh failed"}"#)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        for (status, expected_kind) in [
            (400_u16, HealthFailureKind::AuthFailed),
            (429_u16, HealthFailureKind::RateLimited),
            (500_u16, HealthFailureKind::UpstreamFailed),
        ] {
            let error = refresh_tokens_at("refresh-token", &format!("http://{address}/{status}"))
                .await
                .expect_err("token endpoint should fail");
            assert_eq!(error.status, Some(status));
            assert_eq!(error.kind, expected_kind);
        }

        server.abort();
    }

    #[test]
    fn token_endpoint_error_redacts_echoed_refresh_tokens() {
        let error = oauth_token_endpoint_error(
            reqwest::StatusCode::BAD_REQUEST,
            br#"{"error":"invalid_grant refresh_token=refresh-secret"}"#,
        );

        assert!(!error.message.contains("refresh-secret"));
        assert!(error.message.contains("refresh_token=[redacted]"));
    }
}
