use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use codex_companion_core::atomic_write_private_file;
use codex_companion_daemon::CompanionDaemon;
use codex_companion_provider::ProviderImportOutcome;
use rand::random;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::Url;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const SCOPES: &str = "openid profile email offline_access";
const ORIGINATOR: &str = "codex_vscode";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const PENDING_FILE: &str = "pending.json";
const OAUTH_TIMEOUT_SECONDS: i64 = 300;
const MAX_PENDING_FILE_BYTES: u64 = 64 * 1024;
const MAX_CALLBACK_TARGET_BYTES: usize = 16 * 1024;
const MAX_TOKEN_RESPONSE_BYTES: usize = 1024 * 1024;

static OAUTH_STATE: OnceLock<Mutex<Option<OAuthState>>> = OnceLock::new();
static ACTIVE_LISTENER: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static ACTIVE_COMPLETION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthState {
    login_id: String,
    auth_url: String,
    redirect_uri: String,
    code_verifier: String,
    state: String,
    data_dir: PathBuf,
    expires_at: i64,
    code: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStartResponse {
    pub login_id: String,
    pub auth_url: String,
    pub callback_url: String,
    pub expires_at: i64,
    pub callback_server_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStatusResponse {
    pub login_id: String,
    pub auth_url: String,
    pub callback_url: String,
    pub expires_at: i64,
    pub callback_received: bool,
    pub callback_server_ready: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn state_lock() -> &'static Mutex<Option<OAuthState>> {
    OAUTH_STATE.get_or_init(|| Mutex::new(None))
}

fn listener_lock() -> &'static Mutex<Option<String>> {
    ACTIVE_LISTENER.get_or_init(|| Mutex::new(None))
}

fn completion_lock() -> &'static Mutex<Option<String>> {
    ACTIVE_COMPLETION.get_or_init(|| Mutex::new(None))
}

fn now() -> i64 {
    Utc::now().timestamp()
}

fn pending_path(data_dir: &Path) -> PathBuf {
    data_dir.join("oauth").join(PENDING_FILE)
}

fn ensure_pending_dir(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = data_dir.join("oauth");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("OAuth 状态目录不是可信的本地目录".to_string());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&dir)
                .map_err(|error| format!("创建 OAuth 状态目录失败: {error}"))?;
        }
        Err(error) => return Err(format!("检查 OAuth 状态目录失败: {error}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("设置 OAuth 状态目录权限失败: {error}"))?;
    }
    Ok(dir)
}

fn load_pending(data_dir: &Path) -> Result<Option<OAuthState>, String> {
    let path = pending_path(data_dir);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取 OAuth 状态文件信息失败: {error}")),
    };
    if !metadata.is_file() || metadata.len() > MAX_PENDING_FILE_BYTES {
        quarantine_pending(&path);
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).map_err(|error| format!("读取 OAuth 状态失败: {error}"))?;
    let state = match serde_json::from_str::<OAuthState>(&text) {
        Ok(state) => state,
        Err(_) => {
            quarantine_pending(&path);
            return Ok(None);
        }
    };
    if validate_pending_state(data_dir, &state).is_err() {
        quarantine_pending(&path);
        return Ok(None);
    }
    if state.expires_at <= now() {
        clear_pending(data_dir)?;
        return Ok(None);
    }
    Ok(Some(state))
}

fn quarantine_pending(path: &Path) {
    let quarantine = path.with_file_name(format!("pending.invalid-{}.json", now()));
    if fs::rename(path, quarantine).is_err() {
        let _ = fs::remove_file(path);
    }
}

fn persist_pending(state: Option<&OAuthState>) -> Result<(), String> {
    let Some(state) = state else {
        return Ok(());
    };
    let dir = ensure_pending_dir(&state.data_dir)?;
    let path = dir.join(PENDING_FILE);
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| format!("序列化 OAuth 状态失败: {error}"))?;
    atomic_write_private_file(&path, format!("{text}\n").as_bytes())
        .map_err(|error| format!("写入 OAuth 状态失败: {error}"))
}

fn clear_pending(data_dir: &Path) -> Result<(), String> {
    let path = pending_path(data_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("清理 OAuth 状态失败: {error}")),
    }
}

fn hydrate(data_dir: &Path) -> Result<(), String> {
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    if let Some(current) = guard.as_ref().filter(|state| state.data_dir == data_dir) {
        if current.expires_at > now() {
            return Ok(());
        }
    }
    if let Some(previous) = guard.as_ref() {
        clear_pending(&previous.data_dir)?;
        let login_id = previous.login_id.clone();
        guard.take();
        clear_listener_key(&login_id);
    }
    // Keep the state lock while loading so an expired session cannot clear a
    // pending file created by a concurrent start call.
    *guard = load_pending(data_dir)?;
    Ok(())
}

fn token() -> String {
    URL_SAFE_NO_PAD.encode(random::<[u8; 32]>())
}

fn validate_pending_state(data_dir: &Path, state: &OAuthState) -> Result<(), String> {
    let expected_redirect = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
    let auth_url =
        Url::parse(&state.auth_url).map_err(|_| "OAuth 状态中的授权地址无效".to_string())?;
    let expected_auth_url = build_auth_url(&expected_redirect, &state.code_verifier, &state.state);
    if state.data_dir != data_dir
        || state.redirect_uri != expected_redirect
        || state.login_id.is_empty()
        || state.code_verifier.is_empty()
        || state.state.is_empty()
        || auth_url.scheme() != "https"
        || auth_url.host_str() != Some("auth.openai.com")
        || auth_url.path() != "/oauth/authorize"
        || state.auth_url != expected_auth_url
    {
        return Err("OAuth pending 状态校验失败，请重新开始授权".to_string());
    }
    Ok(())
}

fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn build_auth_url(redirect_uri: &str, verifier: &str, state: &str) -> String {
    let mut url = Url::parse(AUTH_ENDPOINT).expect("valid OAuth endpoint");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &code_challenge(verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", ORIGINATOR);
    url.into()
}

fn response_for(state: &OAuthState, callback_server_ready: bool) -> OAuthStartResponse {
    OAuthStartResponse {
        login_id: state.login_id.clone(),
        auth_url: state.auth_url.clone(),
        callback_url: state.redirect_uri.clone(),
        expires_at: state.expires_at,
        callback_server_ready,
    }
}

fn status_for(state: &OAuthState, callback_server_ready: bool) -> OAuthStatusResponse {
    OAuthStatusResponse {
        login_id: state.login_id.clone(),
        auth_url: state.auth_url.clone(),
        callback_url: state.redirect_uri.clone(),
        expires_at: state.expires_at,
        callback_received: state.code.is_some(),
        callback_server_ready,
        error: state.error.clone(),
    }
}

fn claim_listener(login_id: &str) -> Result<bool, String> {
    let mut guard = listener_lock()
        .lock()
        .map_err(|_| "获取 OAuth 回调监听状态锁失败".to_string())?;
    if guard.as_deref() == Some(login_id) {
        return Ok(false);
    }
    *guard = Some(login_id.to_string());
    Ok(true)
}

fn clear_listener_key(login_id: &str) {
    if let Ok(mut guard) = listener_lock().lock() {
        if guard.as_deref() == Some(login_id) {
            *guard = None;
        }
    }
}

fn spawn_callback_listener(state: &OAuthState) -> bool {
    if state.code.is_some() {
        return true;
    }
    match claim_listener(&state.login_id) {
        Ok(false) => return true,
        Ok(true) => {}
        Err(_) => return false,
    }
    let server = match try_bind_callback_server() {
        Ok(Some(server)) => server,
        Ok(None) | Err(_) => {
            clear_listener_key(&state.login_id);
            return false;
        }
    };
    let expected = state.clone();
    thread::spawn(move || callback_loop(server, expected));
    true
}

fn try_bind_callback_server_on(port: u16) -> Result<Option<Server>, String> {
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => return Ok(None),
        Err(error) => return Err(format!("无法监听 OAuth 回调端口: {error}")),
    };
    Server::from_listener(listener, None)
        .map(Some)
        .map_err(|error| format!("启动 OAuth 回调服务器失败: {error}"))
}

fn try_bind_callback_server() -> Result<Option<Server>, String> {
    try_bind_callback_server_on(CALLBACK_PORT)
}

fn callback_loop(server: Server, expected: OAuthState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(OAUTH_TIMEOUT_SECONDS as u64);
    loop {
        let current = state_lock().lock().ok().and_then(|guard| guard.clone());
        let Some(current) = current else {
            break;
        };
        if current.login_id != expected.login_id
            || current.state != expected.state
            || current.code.is_some()
        {
            break;
        }
        if current.expires_at <= now() || std::time::Instant::now() >= deadline {
            let _ = clear_state_if_matches(&expected);
            break;
        }
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_callback_request(request, &expected),
            Ok(None) => {}
            Err(_) => break,
        }
    }
    clear_listener_key(&expected.login_id);
}

fn handle_callback_request(request: Request, expected: &OAuthState) {
    let result = if request.method() != &Method::Get {
        Err("回调请求方法无效".to_string())
    } else if request.url().len() > MAX_CALLBACK_TARGET_BYTES {
        Err("回调地址过长".to_string())
    } else {
        parse_callback_target(request.url())
            .and_then(|callback| apply_callback(expected, &callback))
    };
    match result {
        Ok(()) => respond_to_callback(
            request,
            200,
            "授权已完成，可以关闭此窗口并返回 Codex Companion。",
        ),
        Err(_) => respond_to_callback(request, 400, "OAuth 回调无效，请返回应用重试。"),
    }
}

fn respond_to_callback(request: Request, status: u16, body: &str) {
    let content_type = Header::from_bytes(
        b"Content-Type".as_slice(),
        b"text/plain; charset=utf-8".as_slice(),
    )
    .expect("static response header is valid");
    let cache_control = Header::from_bytes(b"Cache-Control".as_slice(), b"no-store".as_slice())
        .expect("static response header is valid");
    let no_sniff = Header::from_bytes(b"X-Content-Type-Options".as_slice(), b"nosniff".as_slice())
        .expect("static response header is valid");
    let response = Response::from_string(body.to_string())
        .with_status_code(StatusCode(status))
        .with_header(content_type)
        .with_header(cache_control)
        .with_header(no_sniff);
    let _ = request.respond(response);
}

fn parse_callback_target(target: &str) -> Result<Url, String> {
    if !target.starts_with('/') {
        return Err("回调请求必须使用 origin-form 地址".to_string());
    }
    Url::parse(&format!("http://localhost:{CALLBACK_PORT}{target}"))
        .map_err(|error| format!("回调地址格式无效: {error}"))
}

fn parse_callback_url(value: &str) -> Result<Url, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("回调地址不能为空".to_string());
    }
    if value.len() > MAX_CALLBACK_TARGET_BYTES {
        return Err("回调地址过长".to_string());
    }
    if value.contains("://") {
        return Url::parse(value).map_err(|error| format!("回调地址格式无效: {error}"));
    }
    Url::parse(&format!(
        "http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}?{}",
        value.trim_start_matches('?')
    ))
    .map_err(|error| format!("回调地址格式无效: {error}"))
}

fn validate_callback_url(url: &Url) -> Result<(), String> {
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "http"
        || !matches!(host, "localhost" | "127.0.0.1")
        || url.port_or_known_default() != Some(CALLBACK_PORT)
        || url.path() != CALLBACK_PATH
    {
        return Err(format!(
            "回调地址必须是 http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}（或 127.0.0.1）"
        ));
    }
    Ok(())
}

fn callback_query_value(url: &Url, key: &str) -> Result<Option<String>, String> {
    let mut found = None;
    for (candidate, value) in url.query_pairs() {
        if candidate != key {
            continue;
        }
        if found.is_some() {
            return Err(format!("回调地址包含重复的 {key} 参数"));
        }
        found = Some(value.into_owned());
    }
    Ok(found)
}

fn apply_callback(expected: &OAuthState, callback: &Url) -> Result<(), String> {
    validate_callback_url(callback)?;
    let state = callback_query_value(callback, "state")?.unwrap_or_default();
    if state != expected.state {
        return Err("回调 state 校验失败，请确认使用的是当前授权会话".to_string());
    }
    let callback_error = callback_query_value(callback, "error")?;
    let error_description = callback_query_value(callback, "error_description")?;
    let code = callback_query_value(callback, "code")?;
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    let current = guard
        .as_mut()
        .filter(|current| current.login_id == expected.login_id && current.state == expected.state)
        .ok_or_else(|| "OAuth 会话不存在或已结束".to_string())?;
    if let Some(current_code) = current.code.as_deref() {
        return if code.as_deref().is_some_and(|code| code == current_code) {
            Ok(())
        } else {
            Err("OAuth 回调已完成，不能替换授权码".to_string())
        };
    }
    if let Some(error) = callback_error
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let description = error_description
            .as_deref()
            .map(str::trim)
            .unwrap_or("用户拒绝授权");
        current.error = Some(format!("{error}: {description}"));
        persist_pending(Some(current))?;
        return Err("用户未完成授权".to_string());
    }
    let code = code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "回调地址缺少 code 参数".to_string())?;
    current.code = Some(code.to_string());
    current.error = None;
    persist_pending(Some(current))
}

fn clear_state_if_matches(expected: &OAuthState) -> Result<(), String> {
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    let matches = guard.as_ref().is_some_and(|current| {
        current.login_id == expected.login_id && current.state == expected.state
    });
    if matches {
        let data_dir = guard
            .as_ref()
            .expect("matching OAuth state existed")
            .data_dir
            .clone();
        clear_pending(&data_dir)?;
        let cleared = guard.take().expect("matching OAuth state existed");
        clear_listener_key(&cleared.login_id);
    }
    Ok(())
}

fn clear_state_for_login(data_dir: &Path, login_id: &str) -> Result<(), String> {
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    let Some(current) = guard.as_ref() else {
        return Ok(());
    };
    if current.data_dir != data_dir || current.login_id != login_id {
        return Ok(());
    }
    clear_pending(&current.data_dir)?;
    let state = guard.take().expect("state existed");
    clear_listener_key(&state.login_id);
    Ok(())
}

struct CompletionGuard {
    login_id: String,
}

impl CompletionGuard {
    fn acquire(login_id: &str) -> Result<Self, String> {
        let mut guard = completion_lock()
            .lock()
            .map_err(|_| "获取 OAuth 完成状态锁失败".to_string())?;
        if guard.is_some() {
            return Err("OAuth 账号正在保存，请勿重复提交".to_string());
        }
        *guard = Some(login_id.to_string());
        Ok(Self {
            login_id: login_id.to_string(),
        })
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = completion_lock().lock() {
            if guard.as_deref() == Some(self.login_id.as_str()) {
                *guard = None;
            }
        }
    }
}

pub fn start(data_dir: PathBuf) -> Result<OAuthStartResponse, String> {
    hydrate(&data_dir)?;
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    if let Some(current) = guard.as_ref().filter(|state| state.expires_at > now()) {
        let callback_server_ready = spawn_callback_listener(current);
        return Ok(response_for(current, callback_server_ready));
    }
    let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
    let server = try_bind_callback_server()?;
    let verifier = token();
    let state_token = token();
    let state = OAuthState {
        login_id: token(),
        auth_url: build_auth_url(&redirect_uri, &verifier, &state_token),
        redirect_uri,
        code_verifier: verifier,
        state: state_token,
        data_dir,
        expires_at: now() + OAUTH_TIMEOUT_SECONDS,
        code: None,
        error: None,
    };
    persist_pending(Some(&state))?;
    *guard = Some(state.clone());
    let callback_server_ready = if let Some(server) = server {
        let claimed = claim_listener(&state.login_id)?;
        debug_assert!(claimed, "new OAuth session must own its listener key");
        let listener_state = state.clone();
        thread::spawn(move || callback_loop(server, listener_state));
        true
    } else {
        false
    };
    Ok(response_for(&state, callback_server_ready))
}

pub fn status(data_dir: &Path) -> Result<Option<OAuthStatusResponse>, String> {
    hydrate(data_dir)?;
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    if guard
        .as_ref()
        .is_some_and(|state| state.expires_at <= now())
    {
        let state = guard.as_ref().expect("expired OAuth state existed");
        clear_pending(&state.data_dir)?;
        let login_id = state.login_id.clone();
        guard.take();
        clear_listener_key(&login_id);
    }
    let callback_server_ready = guard
        .as_ref()
        .map(|state| state.code.is_some() || spawn_callback_listener(state))
        .unwrap_or(false);
    Ok(guard
        .as_ref()
        .map(|state| status_for(state, callback_server_ready)))
}

pub fn open_browser(data_dir: &Path, login_id: &str) -> Result<(), String> {
    hydrate(data_dir)?;
    let guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    let state = guard
        .as_ref()
        .ok_or_else(|| "没有进行中的 OAuth 授权".to_string())?;
    if state.login_id != login_id || state.expires_at <= now() {
        return Err("OAuth 授权会话已失效，请重新开始".to_string());
    }
    let url = state.auth_url.clone();
    drop(guard);
    open_external_url(&url)
}

fn open_external_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(url).spawn();
    result
        .map(|_| ())
        .map_err(|error| format!("打开系统浏览器失败: {error}"))
}

pub fn submit_callback(data_dir: &Path, login_id: &str, callback_url: &str) -> Result<(), String> {
    hydrate(data_dir)?;
    let expected = {
        let guard = state_lock()
            .lock()
            .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
        let state = guard
            .as_ref()
            .ok_or_else(|| "没有进行中的 OAuth 授权".to_string())?;
        if state.login_id != login_id {
            return Err("OAuth 授权会话不匹配".to_string());
        }
        state.clone()
    };
    let callback = parse_callback_url(callback_url)?;
    apply_callback(&expected, &callback)
}

pub fn cancel(data_dir: &Path, login_id: Option<&str>) -> Result<(), String> {
    hydrate(data_dir)?;
    let mut guard = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
    let Some(current) = guard.as_ref() else {
        return Ok(());
    };
    if let Some(login_id) = login_id.filter(|value| *value != current.login_id) {
        return Err(format!("OAuth 授权会话不匹配: {login_id}"));
    }
    clear_pending(&current.data_dir)?;
    let state = guard.take().expect("state existed");
    clear_listener_key(&state.login_id);
    Ok(())
}

pub async fn complete(
    daemon: &CompanionDaemon,
    login_id: &str,
) -> Result<ProviderImportOutcome, String> {
    hydrate(&daemon.store().data_dir())?;
    let _completion = CompletionGuard::acquire(login_id)?;
    let (code, verifier, redirect_uri, data_dir) = {
        let guard = state_lock()
            .lock()
            .map_err(|_| "获取 OAuth 状态锁失败".to_string())?;
        let state = guard
            .as_ref()
            .ok_or_else(|| "没有进行中的 OAuth 授权".to_string())?;
        if state.login_id != login_id {
            return Err("OAuth 授权会话不匹配".to_string());
        }
        if state.expires_at <= now() {
            return Err("OAuth 授权已超时，请重新开始".to_string());
        }
        let code = state
            .code
            .clone()
            .ok_or_else(|| "尚未收到 OAuth 回调，请先完成浏览器授权".to_string())?;
        (
            code,
            state.code_verifier.clone(),
            state.redirect_uri.clone(),
            state.data_dir.clone(),
        )
    };
    let tokens = exchange_code(&code, &verifier, &redirect_uri).await?;
    let refresh_token = match tokens
        .refresh_token
        .filter(|value| !value.trim().is_empty())
    {
        Some(refresh_token) => refresh_token,
        None => {
            let _ = clear_state_for_login(&data_dir, login_id);
            return Err("OAuth 响应缺少 refresh_token，无法保存可续期账号".to_string());
        }
    };
    let mut token_map = serde_json::Map::new();
    token_map.insert(
        "access_token".to_string(),
        serde_json::Value::String(tokens.access_token),
    );
    if let Some(id_token) = tokens.id_token {
        token_map.insert("id_token".to_string(), serde_json::Value::String(id_token));
    }
    token_map.insert(
        "refresh_token".to_string(),
        serde_json::Value::String(refresh_token),
    );
    token_map.insert(
        "last_refresh".to_string(),
        serde_json::Value::String(Utc::now().to_rfc3339()),
    );
    let auth = serde_json::json!({
        "tokens": token_map,
        "expired": false,
        "last_refresh": Utc::now().to_rfc3339(),
    });
    let outcome = daemon
        .import_provider_json(&auth.to_string(), None, None)
        .map_err(|error| format!("OAuth 已完成，但保存账号失败，请重新授权: {error}"));
    if let Err(error) = clear_state_for_login(&data_dir, login_id) {
        eprintln!("Codex OAuth pending 状态清理失败: {error}");
    }
    outcome
}

async fn exchange_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthTokenResponse, String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("创建 OAuth 客户端失败: {error}"))?;
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ];
    let mut response = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .await
        .map_err(|error| format!("OAuth token 请求失败: {error}"))?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
    {
        return Err("OAuth token 响应过大".to_string());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取 OAuth token 响应失败: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_TOKEN_RESPONSE_BYTES {
            return Err("OAuth token 响应过大".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(format!(
            "OAuth token 接口返回 {status} [body_len:{}]",
            body.len()
        ));
    }
    let tokens = serde_json::from_slice::<OAuthTokenResponse>(&body)
        .map_err(|error| format!("解析 OAuth token 响应失败: {error}"))?;
    if tokens.access_token.trim().is_empty() {
        return Err("OAuth 响应缺少 access_token".to_string());
    }
    Ok(tokens)
}

pub fn restore_listener(data_dir: PathBuf) -> Result<(), String> {
    hydrate(&data_dir)?;
    let state = state_lock()
        .lock()
        .map_err(|_| "获取 OAuth 状态锁失败".to_string())?
        .clone();
    if let Some(state) = state.filter(|value| value.expires_at > now() && value.code.is_none()) {
        let _ = spawn_callback_listener(&state);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    static OAUTH_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_data_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("codex-companion-oauth-{label}-{}", token()))
    }

    fn test_state(data_dir: PathBuf) -> OAuthState {
        let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");
        let verifier = "test-verifier".to_string();
        let state = "test-state".to_string();
        OAuthState {
            login_id: "test-login".to_string(),
            auth_url: build_auth_url(&redirect_uri, &verifier, &state),
            redirect_uri,
            code_verifier: verifier,
            state,
            data_dir,
            expires_at: now() + 60,
            code: None,
            error: None,
        }
    }

    #[test]
    fn auth_url_contains_pkce_and_state() {
        let url = build_auth_url("http://localhost:1455/auth/callback", "verifier", "state");
        let parsed = Url::parse(&url).expect("url");
        assert_eq!(parsed.host_str(), Some("auth.openai.com"));
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("state").map(|value| value.as_ref()),
            Some("state")
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert!(query
            .get("code_challenge")
            .is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn callback_validation_rejects_non_local_urls() {
        assert!(validate_callback_url(
            &Url::parse("http://localhost:1455/auth/callback?code=x&state=y").unwrap()
        )
        .is_ok());
        assert!(validate_callback_url(
            &Url::parse("https://localhost:1455/auth/callback?code=x&state=y").unwrap()
        )
        .is_err());
        assert!(validate_callback_url(
            &Url::parse("http://example.com:1455/auth/callback?code=x&state=y").unwrap()
        )
        .is_err());
        assert!(validate_callback_url(
            &Url::parse("http://localhost:1456/auth/callback?code=x&state=y").unwrap()
        )
        .is_err());
    }

    #[test]
    fn raw_callback_query_is_normalized_to_fixed_redirect() {
        let url = parse_callback_url("?code=x&state=y").expect("callback");
        assert_eq!(
            url.as_str(),
            "http://localhost:1455/auth/callback?code=x&state=y"
        );
    }

    #[test]
    fn origin_form_callback_target_keeps_the_callback_port() {
        let url = parse_callback_target("/auth/callback?code=x&state=y").expect("callback");
        assert_eq!(url.host_str(), Some("localhost"));
        assert_eq!(url.port(), Some(CALLBACK_PORT));
        assert_eq!(url.path(), CALLBACK_PATH);
    }

    #[test]
    fn http_server_accepts_a_fragmented_origin_form_request_line() {
        let _test_guard = OAUTH_TEST_LOCK.lock().expect("OAuth test lock");
        let data_dir = test_data_dir("fragmented-request");
        let expected = test_state(data_dir.clone());
        {
            let mut guard = state_lock().lock().expect("OAuth state lock");
            *guard = Some(expected.clone());
        }
        let server = try_bind_callback_server_on(0)
            .expect("bind callback server")
            .expect("available ephemeral port");
        let address = server.server_addr().to_ip().expect("TCP server address");
        let handle = thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(2))
                .expect("receive callback request")
                .expect("callback request");
            assert_eq!(request.method(), &Method::Get);
            handle_callback_request(request, &expected);
        });

        let mut stream = TcpStream::connect(address).expect("connect to callback server");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set response timeout");
        stream
            .write_all(b"GET /auth/call")
            .expect("write first request fragment");
        thread::sleep(Duration::from_millis(10));
        stream
            .write_all(b"back?code=x&state=test-state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write second request fragment");
        let mut response_bytes = [0_u8; 256];
        let response_size = stream
            .read(&mut response_bytes)
            .expect("read callback response");
        let response = String::from_utf8_lossy(&response_bytes[..response_size]);
        assert!(response.starts_with("HTTP/1.1 200"));
        handle.join().expect("callback server thread");
        {
            let mut guard = state_lock().lock().expect("OAuth state lock");
            assert_eq!(
                guard.as_ref().and_then(|state| state.code.as_deref()),
                Some("x")
            );
            *guard = None;
        }
        clear_pending(&data_dir).expect("clear pending callback state");
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn full_callback_urls_are_preserved_for_strict_validation() {
        let url = parse_callback_url("https://example.com/auth/callback?code=x&state=y")
            .expect("callback");
        assert_eq!(url.scheme(), "https");
        assert!(validate_callback_url(&url).is_err());
    }

    #[test]
    fn pending_state_round_trips_and_expired_state_is_removed() {
        let data_dir = test_data_dir("round-trip");
        let state = test_state(data_dir.clone());
        persist_pending(Some(&state)).expect("persist pending state");
        let loaded = load_pending(&data_dir)
            .expect("load pending state")
            .expect("pending state");
        assert_eq!(loaded.login_id, state.login_id);

        let mut expired = state;
        expired.expires_at = now() - 1;
        persist_pending(Some(&expired)).expect("persist expired state");
        assert!(load_pending(&data_dir)
            .expect("load expired state")
            .is_none());
        assert!(!pending_path(&data_dir).exists());
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn invalid_pending_state_is_quarantined_without_becoming_active() {
        let data_dir = test_data_dir("invalid");
        let dir = ensure_pending_dir(&data_dir).expect("pending directory");
        fs::write(dir.join(PENDING_FILE), b"{not-json").expect("invalid pending state");

        assert!(load_pending(&data_dir)
            .expect("load invalid state")
            .is_none());
        assert!(!pending_path(&data_dir).exists());
        assert!(fs::read_dir(&dir)
            .expect("read pending directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("pending.invalid-")));
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn tampered_pending_authorization_url_is_quarantined() {
        let data_dir = test_data_dir("tampered-auth-url");
        let mut state = test_state(data_dir.clone());
        state.auth_url.push_str("&prompt=none");
        persist_pending(Some(&state)).expect("persist tampered state");

        assert!(load_pending(&data_dir)
            .expect("load tampered state")
            .is_none());
        assert!(!pending_path(&data_dir).exists());
        let _ = fs::remove_dir_all(data_dir);
    }

    #[cfg(unix)]
    #[test]
    fn pending_symlink_is_quarantined_without_reading_its_target() {
        use std::os::unix::fs::symlink;

        let data_dir = test_data_dir("pending-symlink");
        let dir = ensure_pending_dir(&data_dir).expect("pending directory");
        let target = data_dir.join("external.json");
        fs::write(&target, b"external-secret").expect("seed symlink target");
        symlink(&target, dir.join(PENDING_FILE)).expect("create pending symlink");

        assert!(load_pending(&data_dir)
            .expect("load pending symlink")
            .is_none());
        assert_eq!(
            fs::read(&target).expect("read symlink target"),
            b"external-secret"
        );
        assert!(!pending_path(&data_dir).exists());
        let _ = fs::remove_dir_all(data_dir);
    }

    #[cfg(unix)]
    #[test]
    fn pending_directory_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let data_dir = test_data_dir("pending-dir-symlink");
        let external = data_dir.join("external");
        fs::create_dir_all(&external).expect("external directory");
        symlink(&external, data_dir.join("oauth")).expect("create OAuth directory symlink");

        let error = ensure_pending_dir(&data_dir).expect_err("reject directory symlink");
        assert!(error.contains("不是可信"));
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn callback_state_mismatch_is_rejected() {
        let expected = test_state(test_data_dir("state-mismatch"));
        let callback = Url::parse(&format!(
            "http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}?code=code&state=wrong-state"
        ))
        .expect("callback");
        let error = apply_callback(&expected, &callback).expect_err("mismatched state");
        assert!(error.contains("state"));
    }

    #[test]
    fn duplicate_security_parameters_are_rejected() {
        let expected = test_state(test_data_dir("duplicate-state"));
        let callback = Url::parse(&format!(
            "http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}?code=code&state={}&state={}",
            expected.state, expected.state
        ))
        .expect("callback");
        let error = apply_callback(&expected, &callback).expect_err("duplicate state");
        assert!(error.contains("重复"));
    }

    #[test]
    fn successful_callback_is_idempotent_but_cannot_be_replaced() {
        let _test_guard = OAUTH_TEST_LOCK.lock().expect("OAuth test lock");
        let data_dir = test_data_dir("idempotent-callback");
        let expected = test_state(data_dir.clone());
        {
            let mut guard = state_lock().lock().expect("OAuth state lock");
            *guard = Some(expected.clone());
        }
        let callback = Url::parse(&format!(
            "http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}?code=first-code&state={}",
            expected.state
        ))
        .expect("callback");
        apply_callback(&expected, &callback).expect("first callback");
        apply_callback(&expected, &callback).expect("identical callback retry");

        let replacement = Url::parse(&format!(
            "http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}?code=second-code&state={}",
            expected.state
        ))
        .expect("replacement callback");
        let error = apply_callback(&expected, &replacement).expect_err("replacement callback");
        assert!(error.contains("不能替换"));
        {
            let mut guard = state_lock().lock().expect("OAuth state lock");
            assert_eq!(
                guard.as_ref().and_then(|state| state.code.as_deref()),
                Some("first-code")
            );
            *guard = None;
        }
        clear_pending(&data_dir).expect("clear pending callback state");
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn completion_guard_rejects_parallel_completion() {
        let first = CompletionGuard::acquire("first-login").expect("first completion guard");
        assert!(CompletionGuard::acquire("second-login").is_err());
        drop(first);
        CompletionGuard::acquire("second-login").expect("completion guard after release");
    }

    #[test]
    fn occupied_callback_port_is_reported_as_unavailable() {
        let holder = TcpListener::bind(("127.0.0.1", 0)).expect("ephemeral listener");
        let port = holder.local_addr().expect("listener address").port();
        assert!(try_bind_callback_server_on(port)
            .expect("bind callback server")
            .is_none());
    }
}
