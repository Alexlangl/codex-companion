use crate::http::read_response_bytes_limited;
use base64::{engine::general_purpose, Engine as _};
use chrono::{SecondsFormat, Utc};
use codex_companion_core::{
    atomic_write_private_file, official_auth_mode_from_account, official_auth_mode_from_auth_json,
    provider_relay_auth_ref, redact_sensitive_text, CompanionError, OfficialAuthMode,
    ProviderConfig, Result,
};
use crypto_box::SecretKey;
use ed25519_dalek::{pkcs8::DecodePrivateKey, Signer, SigningKey};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::Mutex;

const AUTH_API_BASE_URL: &str = "https://auth.openai.com/api/accounts";
const TASK_REGISTRATION_RESPONSE_LIMIT_BYTES: usize = 64 * 1024;
static TASK_REGISTRATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct AgentIdentityAuthorization {
    pub header: String,
    pub task_id: String,
}

#[derive(Debug, Clone)]
struct AgentIdentityCredential {
    runtime_id: String,
    private_key: SigningKey,
    encoded_private_key: String,
    task_id: Option<String>,
    auth_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct TaskRegistrationResponse {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default, rename = "taskId")]
    task_id_camel: Option<String>,
    #[serde(default)]
    encrypted_task_id: Option<String>,
    #[serde(default, rename = "encryptedTaskId")]
    encrypted_task_id_camel: Option<String>,
}

pub fn provider_uses_agent_identity(provider: &ProviderConfig) -> bool {
    if provider.kind != codex_companion_core::ProviderKind::OfficialCodex {
        return false;
    }
    if let Some(mode) = auth_file_mode(provider) {
        return mode == OfficialAuthMode::AgentIdentity;
    }
    official_auth_mode_from_account(provider) == Some(OfficialAuthMode::AgentIdentity)
}

pub async fn ensure_agent_identity_authorization(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    expected_invalid_task_id: Option<&str>,
) -> Result<AgentIdentityAuthorization> {
    ensure_agent_identity_authorization_at(
        client,
        provider,
        expected_invalid_task_id,
        AUTH_API_BASE_URL,
    )
    .await
}

async fn ensure_agent_identity_authorization_at(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    expected_invalid_task_id: Option<&str>,
    auth_api_base_url: &str,
) -> Result<AgentIdentityAuthorization> {
    let lock = TASK_REGISTRATION_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().await;
    let mut credential = load_agent_identity_credential(provider)?;
    let invalid_task_id = expected_invalid_task_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let current_task_id = credential.task_id.as_deref().map(str::trim);
    let should_register = current_task_id.is_none_or(str::is_empty)
        || invalid_task_id.is_some_and(|invalid| current_task_id == Some(invalid));
    if should_register {
        let task_id = register_task(client, &credential, auth_api_base_url).await?;
        persist_task_id(&credential, &task_id)?;
        credential.task_id = Some(task_id);
    }
    build_authorization(&credential)
}

pub fn is_agent_identity_task_invalid(status: reqwest::StatusCode, body: &str) -> bool {
    if status != reqwest::StatusCode::UNAUTHORIZED {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    [
        "\"code\":\"invalid_task_id\"",
        "\"code\":\"task_not_found\"",
        "\"code\":\"task_expired\"",
        "\"error\":\"invalid_task_id\"",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
        || [
            "invalid task_id",
            "invalid task id",
            "task_id is invalid",
            "task id is invalid",
            "task not found",
            "task expired",
            "unknown task_id",
            "unknown task id",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

pub fn redact_agent_identity_body(provider: &ProviderConfig, body: &str) -> String {
    let mut redacted = redact_assertions(body);
    let Ok(credential) = load_agent_identity_credential(provider) else {
        return redact_sensitive_text(&redacted);
    };
    for value in [
        Some(credential.runtime_id.as_str()),
        Some(credential.encoded_private_key.as_str()),
        credential.task_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    {
        redacted = redacted.replace(value, "[redacted]");
    }
    redact_sensitive_text(&redacted)
}

fn load_agent_identity_credential(provider: &ProviderConfig) -> Result<AgentIdentityCredential> {
    let auth_path = provider_relay_auth_ref(provider)
        .and_then(|auth_ref| auth_ref.strip_prefix("file:"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity provider 缺少 auth 文件".to_string())
        })?;
    let value = read_auth_value(&auth_path)?;
    if official_auth_mode_from_auth_json(&value) != Some(OfficialAuthMode::AgentIdentity) {
        return Err(CompanionError::InvalidConfig(
            "provider 不是 Agent Identity 认证".to_string(),
        ));
    }
    let runtime_id =
        json_string(&value, &["agent_runtime_id", "agentRuntimeId"]).ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity 缺少 agent_runtime_id".to_string())
        })?;
    let encoded_private_key = json_string(&value, &["agent_private_key", "agentPrivateKey"])
        .ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity 缺少 agent_private_key".to_string())
        })?;
    let private_key_der = general_purpose::STANDARD
        .decode(encoded_private_key.trim())
        .map_err(|_| {
            CompanionError::InvalidConfig("Agent Identity 私钥不是有效 Base64".to_string())
        })?;
    let private_key = SigningKey::from_pkcs8_der(&private_key_der).map_err(|_| {
        CompanionError::InvalidConfig(
            "Agent Identity 私钥不是有效的 PKCS#8 Ed25519 私钥".to_string(),
        )
    })?;
    Ok(AgentIdentityCredential {
        runtime_id,
        private_key,
        encoded_private_key,
        task_id: json_string(&value, &["task_id", "taskId"]),
        auth_path,
    })
}

fn auth_file_mode(provider: &ProviderConfig) -> Option<OfficialAuthMode> {
    let auth_path = provider_relay_auth_ref(provider)
        .and_then(|auth_ref| auth_ref.strip_prefix("file:"))
        .map(PathBuf::from)?;
    let value = read_auth_value(&auth_path).ok()?;
    official_auth_mode_from_auth_json(&value)
}

fn read_auth_value(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
    serde_json::from_str(&text).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "解析 Agent Identity auth 文件失败 {}: {source}",
            path.display()
        ))
    })
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn build_authorization(credential: &AgentIdentityCredential) -> Result<AgentIdentityAuthorization> {
    let task_id = credential
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CompanionError::InvalidConfig("Agent Identity task_id 为空".to_string()))?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let payload = format!("{}:{}:{timestamp}", credential.runtime_id, task_id);
    let signature = credential.private_key.sign(payload.as_bytes());
    let envelope = serde_json::json!({
        "agent_runtime_id": credential.runtime_id,
        "task_id": task_id,
        "timestamp": timestamp,
        "signature": general_purpose::STANDARD.encode(signature.to_bytes()),
    });
    let bytes = serde_json::to_vec(&envelope).map_err(|source| {
        CompanionError::InvalidConfig(format!("AgentAssertion serialize failed: {source}"))
    })?;
    Ok(AgentIdentityAuthorization {
        header: format!(
            "AgentAssertion {}",
            general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        ),
        task_id: task_id.to_string(),
    })
}

async fn register_task(
    client: &reqwest::Client,
    credential: &AgentIdentityCredential,
    auth_api_base_url: &str,
) -> Result<String> {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let payload = format!("{}:{timestamp}", credential.runtime_id);
    let signature = credential.private_key.sign(payload.as_bytes());
    let url = format!(
        "{auth_api_base_url}/v1/agent/{}/task/register",
        credential.runtime_id
    );
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "timestamp": timestamp,
            "signature": general_purpose::STANDARD.encode(signature.to_bytes()),
        }))
        .send()
        .await
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("Agent Identity task 注册请求失败: {source}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(CompanionError::InvalidConfig(format!(
            "Agent Identity task 注册返回 HTTP {status}"
        )));
    }
    let bytes = read_response_bytes_limited(response, TASK_REGISTRATION_RESPONSE_LIMIT_BYTES)
        .await
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("读取 Agent Identity task 响应失败: {source}"))
        })?;
    let result: TaskRegistrationResponse = serde_json::from_slice(&bytes).map_err(|source| {
        CompanionError::InvalidConfig(format!("Agent Identity task 注册响应格式无效: {source}"))
    })?;
    if let Some(task_id) = result
        .task_id
        .or(result.task_id_camel)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(task_id);
    }
    let encrypted = result
        .encrypted_task_id
        .or(result.encrypted_task_id_camel)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity task 注册响应缺少 task_id".to_string())
        })?;
    decrypt_task_id(credential, &encrypted)
}

fn decrypt_task_id(credential: &AgentIdentityCredential, encoded: &str) -> Result<String> {
    let ciphertext = general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| {
            CompanionError::InvalidConfig("加密 Agent Identity task_id 不是有效 Base64".to_string())
        })?;
    let digest = Sha512::digest(credential.private_key.to_bytes());
    let mut curve_private = [0u8; 32];
    curve_private.copy_from_slice(&digest[..32]);
    let plaintext = SecretKey::from_bytes(curve_private)
        .unseal(&ciphertext)
        .map_err(|_| {
            CompanionError::InvalidConfig("解密 Agent Identity task_id 失败".to_string())
        })?;
    let task_id = String::from_utf8(plaintext)
        .map_err(|_| {
            CompanionError::InvalidConfig(
                "解密后的 Agent Identity task_id 不是有效 UTF-8".to_string(),
            )
        })?
        .trim()
        .to_string();
    if task_id.is_empty() {
        return Err(CompanionError::InvalidConfig(
            "解密后的 Agent Identity task_id 为空".to_string(),
        ));
    }
    Ok(task_id)
}

fn persist_task_id(credential: &AgentIdentityCredential, task_id: &str) -> Result<()> {
    crate::with_private_auth_file_lock(&credential.auth_path, || {
        let mut value = read_auth_value(&credential.auth_path)?;
        if json_string(&value, &["agent_runtime_id", "agentRuntimeId"]).as_deref()
            != Some(credential.runtime_id.as_str())
            || json_string(&value, &["agent_private_key", "agentPrivateKey"]).as_deref()
                != Some(credential.encoded_private_key.as_str())
        {
            return Err(CompanionError::InvalidConfig(
                "Agent Identity 凭据在 task 注册期间发生变化".to_string(),
            ));
        }
        value["task_id"] = Value::String(task_id.to_string());
        let text = serde_json::to_string_pretty(&value).map_err(|source| {
            CompanionError::InvalidConfig(format!("Agent Identity task serialize failed: {source}"))
        })?;
        atomic_write_private_file(&credential.auth_path, format!("{text}\n").as_bytes())
    })
}

fn redact_assertions(body: &str) -> String {
    let prefix = "AgentAssertion ";
    let mut redacted = body.to_string();
    let mut offset = 0;
    while let Some(relative_start) = redacted[offset..].find(prefix) {
        let start = offset + relative_start;
        let value_start = start + prefix.len();
        let end = redacted[value_start..]
            .find(|character: char| character.is_ascii_whitespace() || "\"',}".contains(character))
            .map(|end_offset| value_start + end_offset)
            .unwrap_or(redacted.len());
        redacted.replace_range(value_start..end, "[redacted]");
        offset = value_start + "[redacted]".len();
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::post, Json, Router};
    use crypto_box::aead::OsRng;
    use ed25519_dalek::{pkcs8::EncodePrivateKey, Verifier};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn file_mode_overrides_stale_agent_identity_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let auth_path = temp.path().join("auth.json");
        fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"oauth-access","refresh_token":"oauth-refresh"}}"#,
        )
        .expect("auth");
        let provider = ProviderConfig {
            id: "oauth".to_string(),
            name: "OAuth".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: Some(codex_companion_core::ProviderAccountInfo {
                auth_mode: Some("agentIdentity".to_string()),
                ..Default::default()
            }),
        };

        assert!(!provider_uses_agent_identity(&provider));
    }

    #[test]
    fn declared_agent_identity_stays_relay_only_when_credential_is_incomplete() {
        let temp = tempfile::tempdir().expect("temp");
        let auth_path = temp.path().join("auth.json");
        fs::write(&auth_path, r#"{"auth_mode":"agentIdentity"}"#).expect("auth");
        let provider = ProviderConfig {
            id: "agent".to_string(),
            name: "Agent".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };

        assert!(provider_uses_agent_identity(&provider));
    }

    #[test]
    fn assertion_contains_a_verifiable_signature() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let temp = tempfile::tempdir().expect("temp");
        let auth_path = temp.path().join("auth.json");
        let encoded =
            general_purpose::STANDARD.encode(signing_key.to_pkcs8_der().expect("pkcs8").as_bytes());
        fs::write(
            &auth_path,
            serde_json::json!({
                "auth_mode": "agentIdentity",
                "agent_runtime_id": "runtime-1",
                "agent_private_key": encoded,
                "task_id": "task-1"
            })
            .to_string(),
        )
        .expect("auth");
        let provider = ProviderConfig {
            id: "agent".to_string(),
            name: "Agent".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };
        let credential = load_agent_identity_credential(&provider).expect("credential");
        let authorization = build_authorization(&credential).expect("assertion");
        let encoded = authorization.header.trim_start_matches("AgentAssertion ");
        let envelope: Value = serde_json::from_slice(
            &general_purpose::URL_SAFE_NO_PAD
                .decode(encoded)
                .expect("decode envelope"),
        )
        .expect("envelope");
        let timestamp = envelope["timestamp"].as_str().expect("timestamp");
        let payload = format!("runtime-1:task-1:{timestamp}");
        let signature = general_purpose::STANDARD
            .decode(envelope["signature"].as_str().expect("signature"))
            .expect("signature bytes");
        let signature = ed25519_dalek::Signature::from_slice(&signature).expect("signature");
        signing_key
            .verifying_key()
            .verify(payload.as_bytes(), &signature)
            .expect("valid signature");
    }

    #[tokio::test]
    async fn invalid_task_is_registered_once_and_persisted() {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/agent/runtime-1/task/register",
                post(
                    |State(hits): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        assert!(body.get("timestamp").and_then(Value::as_str).is_some());
                        assert!(body.get("signature").and_then(Value::as_str).is_some());
                        Json(serde_json::json!({ "task_id": "task-new" }))
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let signing_key = SigningKey::generate(&mut OsRng);
        let temp = tempfile::tempdir().expect("temp");
        let auth_path = temp.path().join("auth.json");
        let encoded =
            general_purpose::STANDARD.encode(signing_key.to_pkcs8_der().expect("pkcs8").as_bytes());
        fs::write(
            &auth_path,
            serde_json::json!({
                "auth_mode": "agentIdentity",
                "agent_runtime_id": "runtime-1",
                "agent_private_key": encoded,
                "task_id": "task-old"
            })
            .to_string(),
        )
        .expect("auth");
        let provider = ProviderConfig {
            id: "agent".to_string(),
            name: "Agent".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };
        let auth_api_base_url = format!("http://{addr}");

        let recovered = ensure_agent_identity_authorization_at(
            &reqwest::Client::new(),
            &provider,
            Some("task-old"),
            &auth_api_base_url,
        )
        .await
        .expect("recover task");
        let reused = ensure_agent_identity_authorization_at(
            &reqwest::Client::new(),
            &provider,
            Some("task-old"),
            &auth_api_base_url,
        )
        .await
        .expect("reuse task");

        assert_eq!(recovered.task_id, "task-new");
        assert_eq!(reused.task_id, "task-new");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let persisted: Value =
            serde_json::from_str(&fs::read_to_string(&auth_path).expect("persisted auth"))
                .expect("persisted json");
        assert_eq!(persisted["task_id"], "task-new");
        assert!(!auth_path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&auth_path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_task_detection_requires_an_unauthorized_task_error() {
        assert!(is_agent_identity_task_invalid(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"task_expired"}}"#
        ));
        assert!(is_agent_identity_task_invalid(
            reqwest::StatusCode::UNAUTHORIZED,
            "task not found"
        ));
        assert!(!is_agent_identity_task_invalid(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"task_expired"}}"#
        ));
        assert!(!is_agent_identity_task_invalid(
            reqwest::StatusCode::UNAUTHORIZED,
            "token expired"
        ));
    }

    #[test]
    fn unreadable_credential_still_redacts_agent_assertions_and_tokens() {
        let provider = ProviderConfig {
            id: "agent".to_string(),
            name: "Agent".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some("file:/missing/agent-auth.json".to_string()),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };

        let body =
            "upstream echoed AgentAssertion assertion-secret and refresh_token=refresh-secret";
        let redacted = redact_agent_identity_body(&provider, body);

        assert!(!redacted.contains("assertion-secret"));
        assert!(!redacted.contains("refresh-secret"));
        assert!(redacted.contains("AgentAssertion [redacted]"));
    }

    #[test]
    fn decrypts_an_encrypted_task_id_for_the_identity_key() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let temp = tempfile::tempdir().expect("temp");
        let auth_path = temp.path().join("auth.json");
        let encoded =
            general_purpose::STANDARD.encode(signing_key.to_pkcs8_der().expect("pkcs8").as_bytes());
        fs::write(
            &auth_path,
            serde_json::json!({
                "auth_mode": "agentIdentity",
                "agent_runtime_id": "runtime-1",
                "agent_private_key": encoded,
                "task_id": "task-old"
            })
            .to_string(),
        )
        .expect("auth");
        let provider = ProviderConfig {
            id: "agent".to_string(),
            name: "Agent".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 1,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };
        let credential = load_agent_identity_credential(&provider).expect("credential");
        let digest = Sha512::digest(signing_key.to_bytes());
        let mut curve_private = [0u8; 32];
        curve_private.copy_from_slice(&digest[..32]);
        let ciphertext = SecretKey::from_bytes(curve_private)
            .public_key()
            .seal(&mut OsRng, b"task-encrypted")
            .expect("encrypt");
        let encoded = general_purpose::STANDARD.encode(ciphertext);

        assert_eq!(
            decrypt_task_id(&credential, &encoded).expect("decrypt"),
            "task-encrypted"
        );
    }
}
