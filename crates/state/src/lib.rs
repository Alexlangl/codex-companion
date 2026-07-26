mod pricing;
mod session_index;
mod token_usage;

use chrono::Local;
use codex_companion_core::{
    default_codex_dir, CodexInstallStatus, CompanionError, ProviderConfig, ProviderKind,
    RelayConfig, RepairOptions, RepairOutcome, RepairPlan, Result, COMPANION_PROVIDER_ID,
    COMPANION_PROVIDER_NAME,
};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item};
use walkdir::WalkDir;

const CODEX_STATE_DB_FILENAME: &str = "state_5.sqlite";
const CODEX_OPENAI_PROVIDER_ID: &str = "openai";
const CODEX_API_KEY_AUTH_MODE: &str = "apikey";
#[cfg(all(target_os = "macos", not(test)))]
const CODEX_KEYCHAIN_SERVICE: &str = "Codex Auth";
const COMPANION_MARKER_TABLE: &str = "codex_companion";
const COMPANION_MARKER_VERSION: i64 = 1;
const REPAIR_BACKUP_RETENTION: usize = 10;

pub use session_index::list_sessions_cached;
pub use token_usage::{
    collect_token_usage, collect_token_usage_cached, collect_token_usage_cached_in_range,
    collect_token_usage_cached_with_filters, rebuild_token_usage_cached_with_filters,
    token_usage_sync_status, TokenUsageDateRange, TokenUsageFilters,
};

#[derive(Debug, Clone, Default)]
struct SessionMetadata {
    cwd: Option<String>,
    rollout_path: Option<String>,
}

pub fn install_companion_provider(
    codex_dir: Option<PathBuf>,
    relay: &RelayConfig,
) -> Result<CodexInstallStatus> {
    install_companion_provider_with_token_source(codex_dir, relay, None)
}

pub fn install_companion_provider_with_token_source(
    codex_dir: Option<PathBuf>,
    relay: &RelayConfig,
    token_source_override: Option<&str>,
) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    fs::create_dir_all(&codex_dir).map_err(|source| CompanionError::io(&codex_dir, source))?;
    let config_path = codex_dir.join("config.toml");
    let current = if config_path.exists() {
        fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?
    } else {
        String::new()
    };
    let mut doc = current.parse::<DocumentMut>().map_err(|source| {
        CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
    })?;
    let managed_target_provider =
        CompanionConfigMarker::from_doc(&doc).and_then(|marker| marker.target_provider);
    let auth_rollback = AuthRollback::capture(&codex_dir)?;
    let mut backup = prepare_config_write(&codex_dir, &config_path, &doc)?;
    restore_prior_auth_write_if_managed(&mut backup, &codex_dir)?;
    let auth_shape = detect_codex_auth_shape(&codex_dir)?;

    remove_previous_managed_provider_table(
        &mut doc,
        managed_target_provider.as_deref(),
        COMPANION_PROVIDER_ID,
    );
    doc["model_provider"] = value(COMPANION_PROVIDER_ID);
    doc["model_providers"][COMPANION_PROVIDER_ID]["name"] = value(COMPANION_PROVIDER_NAME);
    doc["model_providers"][COMPANION_PROVIDER_ID]["base_url"] = value(relay.base_url());
    doc["model_providers"][COMPANION_PROVIDER_ID]["wire_api"] = value("responses");
    apply_companion_marker(
        &mut doc,
        &backup,
        CompanionInstallKind::Relay,
        COMPANION_PROVIDER_ID,
        token_source_override.unwrap_or_else(|| auth_shape.relay_token_source()),
    );

    write_config_with_auth_rollback(&config_path, &doc, &auth_rollback, &codex_dir)?;
    let mut status = doctor(codex_dir, relay)?;
    status.message = format!("{}，auth.json 未被本地代理写入", status.message);
    Ok(status)
}

pub fn install_direct_provider(
    codex_dir: Option<PathBuf>,
    provider: &ProviderConfig,
) -> Result<CodexInstallStatus> {
    install_direct_provider_with_options(codex_dir, provider, DirectInstallOptions::default())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DirectInstallOptions {
    pub preserve_official_codex_auth: bool,
}

pub fn install_direct_provider_with_options(
    codex_dir: Option<PathBuf>,
    provider: &ProviderConfig,
    options: DirectInstallOptions,
) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    fs::create_dir_all(&codex_dir).map_err(|source| CompanionError::io(&codex_dir, source))?;
    let config_path = codex_dir.join("config.toml");
    let current = if config_path.exists() {
        fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?
    } else {
        String::new()
    };
    let mut doc = current.parse::<DocumentMut>().map_err(|source| {
        CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
    })?;
    let managed_target_provider =
        CompanionConfigMarker::from_doc(&doc).and_then(|marker| marker.target_provider);
    let auth_rollback = AuthRollback::capture(&codex_dir)?;
    let mut backup = prepare_config_write(&codex_dir, &config_path, &doc)?;
    let auth_shape_before = detect_codex_auth_shape(&codex_dir)?;
    let direct_auth = resolve_direct_auth(provider)?;
    let mut token_source = direct_auth.token_source();
    let preserve_api_key_in_config = options.preserve_official_codex_auth
        && !matches!(provider.kind, ProviderKind::OfficialCodex)
        && matches!(&direct_auth, DirectAuthMaterial::ApiKey(_));
    let direct_writes_auth = direct_auth.writes_auth_json() && !preserve_api_key_in_config;
    let mut restored_prior_auth_write = false;

    let mut effective_model_provider = provider.id.clone();
    let mut keychain_warning = None;
    if matches!(provider.kind, ProviderKind::OfficialCodex) {
        let DirectAuthMaterial::CodexAuth(auth) = direct_auth else {
            return Err(CompanionError::InvalidConfig(format!(
                "官方 Codex 账号 {} 缺少可直连的 OAuth access_token",
                provider.name
            )));
        };
        write_official_codex_config(&mut doc, managed_target_provider.as_deref());
        ensure_managed_auth_write_unchanged(&backup, &codex_dir)?;
        ensure_auth_backup(&mut backup, &codex_dir)?;
        write_codex_auth_json(&codex_dir, &auth)?;
        record_auth_write(&mut backup, &codex_dir)?;
        if let Err(error) = write_codex_keychain_to_dir(&codex_dir, &auth) {
            keychain_warning = Some(format!(
                "；Keychain 写入失败，若 Codex Desktop 仍要求登录，请检查 macOS 钥匙串权限: {error}"
            ));
        }
        effective_model_provider = CODEX_OPENAI_PROVIDER_ID.to_string();
    } else {
        remove_previous_managed_provider_table(
            &mut doc,
            managed_target_provider.as_deref(),
            &provider.id,
        );
        doc["model_provider"] = value(&provider.id);
        doc["model_providers"][&provider.id]["name"] = value(&provider.name);
        doc["model_providers"][&provider.id]["base_url"] = value(&provider.base_url);
        doc["model_providers"][&provider.id]["wire_api"] = value("responses");
        doc["model_providers"][&provider.id]["requires_openai_auth"] = value(true);
        doc["model_providers"][&provider.id]["api_key_env_var"] = Item::None;

        match direct_auth {
            DirectAuthMaterial::EnvKey(env_var) => {
                restored_prior_auth_write =
                    restore_prior_auth_write_if_managed(&mut backup, &codex_dir)?;
                token_source = format!("environment variable {env_var}");
                doc["model_providers"][&provider.id]["env_key"] = value(env_var);
                doc["model_providers"][&provider.id]["experimental_bearer_token"] = Item::None;
            }
            DirectAuthMaterial::ApiKey(api_key) => {
                doc["model_providers"][&provider.id]["env_key"] = Item::None;
                if preserve_api_key_in_config {
                    restored_prior_auth_write =
                        restore_prior_auth_write_if_managed(&mut backup, &codex_dir)?;
                    doc["model_providers"][&provider.id]["experimental_bearer_token"] =
                        value(api_key);
                    token_source = "provider-scoped experimental_bearer_token in Codex config.toml; official ChatGPT OAuth auth.json preserved"
                        .to_string();
                } else {
                    doc["model_providers"][&provider.id]["experimental_bearer_token"] = Item::None;
                    ensure_managed_auth_write_unchanged(&backup, &codex_dir)?;
                    ensure_auth_backup(&mut backup, &codex_dir)?;
                    write_codex_openai_api_key(&codex_dir, &api_key)?;
                    record_auth_write(&mut backup, &codex_dir)?;
                }
            }
            DirectAuthMaterial::CodexAuth(auth) => {
                doc["model_providers"][&provider.id]["env_key"] = Item::None;
                doc["model_providers"][&provider.id]["experimental_bearer_token"] = Item::None;
                ensure_managed_auth_write_unchanged(&backup, &codex_dir)?;
                ensure_auth_backup(&mut backup, &codex_dir)?;
                write_codex_auth_json(&codex_dir, &auth)?;
                record_auth_write(&mut backup, &codex_dir)?;
            }
            DirectAuthMaterial::None => {
                restored_prior_auth_write =
                    restore_prior_auth_write_if_managed(&mut backup, &codex_dir)?;
                doc["model_providers"][&provider.id]["env_key"] = Item::None;
                doc["model_providers"][&provider.id]["experimental_bearer_token"] = Item::None;
            }
        }
    }
    if !direct_writes_auth && restored_prior_auth_write {
        token_source.push_str("; any prior Companion auth.json write was restored first");
    }
    apply_companion_marker(
        &mut doc,
        &backup,
        CompanionInstallKind::Direct,
        &effective_model_provider,
        &token_source,
    );

    write_config_with_auth_rollback(&config_path, &doc, &auth_rollback, &codex_dir)?;
    let auth_shape_after = detect_codex_auth_shape(&codex_dir)?;
    let auth_warning = direct_auth_warning(&auth_shape_before, &auth_shape_after, &token_source);
    let mut message = format!(
        "Codex 已直连 provider: {}；Token source: {}{}",
        provider.name, token_source, auth_warning
    );
    if let Some(warning) = keychain_warning {
        message.push_str(&warning);
    }
    Ok(CodexInstallStatus {
        codex_dir,
        config_path,
        installed: true,
        model_provider: Some(effective_model_provider),
        companion_base_url: provider.base_url.clone(),
        message,
    })
}

enum DirectAuthMaterial {
    EnvKey(String),
    ApiKey(String),
    CodexAuth(Value),
    None,
}

impl DirectAuthMaterial {
    fn token_source(&self) -> String {
        match self {
            DirectAuthMaterial::EnvKey(env_var) => format!("environment variable {env_var}"),
            DirectAuthMaterial::ApiKey(_) => "API key file copied into Codex auth.json".to_string(),
            DirectAuthMaterial::CodexAuth(_) => {
                "official Codex OAuth auth file merged into Codex auth.json".to_string()
            }
            DirectAuthMaterial::None => {
                "existing Codex auth.json or Codex default auth resolution".to_string()
            }
        }
    }

    fn writes_auth_json(&self) -> bool {
        matches!(
            self,
            DirectAuthMaterial::ApiKey(_) | DirectAuthMaterial::CodexAuth(_)
        )
    }
}

fn resolve_direct_auth(provider: &ProviderConfig) -> Result<DirectAuthMaterial> {
    let Some(auth_ref) = provider
        .direct_auth_ref
        .as_deref()
        .or(provider.auth_ref.as_deref())
        .map(str::trim)
        .filter(|auth_ref| !auth_ref.is_empty())
    else {
        return Ok(DirectAuthMaterial::None);
    };
    if let Some(env_var) = auth_ref
        .strip_prefix("env:")
        .map(str::trim)
        .filter(|env_var| !env_var.is_empty())
    {
        return Ok(DirectAuthMaterial::EnvKey(env_var.to_string()));
    }
    if let Some(path) = auth_ref.strip_prefix("file:") {
        let path = PathBuf::from(path);
        let text = fs::read_to_string(&path).map_err(|source| CompanionError::io(&path, source))?;
        let value = serde_json::from_str::<Value>(&text).map_err(|source| {
            CompanionError::InvalidConfig(format!(
                "解析 provider auth 文件失败 {}: {source}",
                path.display()
            ))
        })?;
        if matches!(provider.kind, ProviderKind::OfficialCodex) {
            let auth = normalize_codex_oauth_auth(&value).ok_or_else(|| {
                CompanionError::InvalidConfig(format!(
                    "官方 Codex 账号 auth 文件缺少 access_token: {}",
                    path.display()
                ))
            })?;
            return Ok(DirectAuthMaterial::CodexAuth(auth));
        }
        let api_key = pick_json_string(
            &value,
            &[
                &["OPENAI_API_KEY"],
                &["api_key"],
                &["openai_api_key"],
                &["credentials", "api_key"],
                &["tokens", "api_key"],
            ],
        )
        .ok_or_else(|| {
            CompanionError::InvalidConfig(format!(
                "API Key auth 文件缺少 OPENAI_API_KEY/api_key: {}",
                path.display()
            ))
        })?;
        return Ok(DirectAuthMaterial::ApiKey(api_key));
    }
    Ok(DirectAuthMaterial::None)
}

fn normalize_codex_oauth_auth(value: &Value) -> Option<Value> {
    let null = Value::Null;
    let candidate = oauth_account_candidate(value);
    let tokens_source = candidate
        .get("tokens")
        .or_else(|| value.get("tokens"))
        .unwrap_or(&null);
    let credentials_source = candidate
        .get("credentials")
        .or_else(|| value.get("credentials"))
        .unwrap_or(&null);
    let extra_source = candidate
        .get("extra")
        .or_else(|| value.get("extra"))
        .unwrap_or(&null);
    let auth_source = value.get("auth").unwrap_or(&null);
    let sources = [
        tokens_source,
        credentials_source,
        extra_source,
        auth_source,
        candidate,
        value,
    ];

    let access_token = pick_first_json_string(
        &sources,
        &[
            &["access_token"],
            &["accessToken"],
            &["token"],
            &["credentials", "access_token"],
            &["tokens", "access_token"],
        ],
    );
    let id_token = pick_first_json_string(
        &sources,
        &[
            &["id_token"],
            &["idToken"],
            &["credentials", "id_token"],
            &["tokens", "id_token"],
        ],
    );
    let session_token = pick_first_json_string(
        &sources,
        &[
            &["session_token"],
            &["sessionToken"],
            &["credentials", "session_token"],
            &["tokens", "session_token"],
        ],
    );
    let refresh_token = pick_first_json_string(
        &sources,
        &[
            &["refresh_token"],
            &["refreshToken"],
            &["credentials", "refresh_token"],
            &["tokens", "refresh_token"],
        ],
    );
    access_token.as_ref()?;

    let mut tokens = tokens_source
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    insert_optional_json_string(&mut tokens, "access_token", access_token);
    insert_optional_json_string(&mut tokens, "id_token", id_token);
    insert_optional_json_string(&mut tokens, "session_token", session_token);
    insert_optional_json_string(&mut tokens, "refresh_token", refresh_token);

    let account_id = pick_first_json_string(
        &sources,
        &[
            &["chatgpt_account_id"],
            &["account_id"],
            &["accountId"],
            &["workspace_id"],
            &["credentials", "chatgpt_account_id"],
            &["tokens", "chatgpt_account_id"],
        ],
    );
    insert_optional_json_string(&mut tokens, "account_id", account_id.clone());
    insert_optional_json_string(&mut tokens, "chatgpt_account_id", account_id);
    insert_optional_json_string(
        &mut tokens,
        "email",
        pick_first_json_string(
            &sources,
            &[
                &["email"],
                &["name"],
                &["credentials", "email"],
                &["tokens", "email"],
            ],
        ),
    );
    insert_optional_json_string(
        &mut tokens,
        "name",
        pick_first_json_string(
            &sources,
            &[
                &["name"],
                &["display_name"],
                &["displayName"],
                &["tokens", "name"],
            ],
        ),
    );
    insert_optional_json_string(
        &mut tokens,
        "plan_type",
        pick_first_json_string(
            &sources,
            &[
                &["plan_type"],
                &["planType"],
                &["chatgpt_plan_type"],
                &["credentials", "plan_type"],
                &["tokens", "plan_type"],
            ],
        ),
    );

    let mut auth = serde_json::Map::new();
    auth.insert("OPENAI_API_KEY".to_string(), Value::Null);
    auth.insert("tokens".to_string(), Value::Object(tokens));
    insert_optional_json_string(
        &mut auth,
        "expired",
        pick_first_json_string(&sources, &[&["expired"], &["expires_at"], &["expiresAt"]]),
    );
    insert_optional_json_string(
        &mut auth,
        "last_refresh",
        pick_first_json_string(&sources, &[&["last_refresh"], &["lastRefresh"]]),
    );
    Some(Value::Object(auth))
}

fn write_official_codex_config(doc: &mut DocumentMut, managed_target_provider: Option<&str>) {
    doc["openai_base_url"] = Item::None;
    doc["model_provider"] = value(CODEX_OPENAI_PROVIDER_ID);
    remove_previous_managed_provider_table(doc, managed_target_provider, CODEX_OPENAI_PROVIDER_ID);
}

fn remove_previous_managed_provider_table(
    doc: &mut DocumentMut,
    managed_target_provider: Option<&str>,
    next_provider: &str,
) {
    let Some(previous_provider) = managed_target_provider else {
        return;
    };
    if previous_provider == next_provider || previous_provider == CODEX_OPENAI_PROVIDER_ID {
        return;
    }
    remove_model_provider_table(doc, previous_provider);
}

fn remove_model_provider_table(doc: &mut DocumentMut, provider_id: &str) {
    let Some(model_providers) = doc.get_mut("model_providers").and_then(Item::as_table_mut) else {
        return;
    };
    model_providers.remove(provider_id);
    model_providers.remove(CODEX_OPENAI_PROVIDER_ID);
    if model_providers.is_empty() {
        doc["model_providers"] = Item::None;
    }
}

fn oauth_account_candidate(value: &Value) -> &Value {
    value
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|accounts| {
            accounts
                .iter()
                .find(|account| {
                    account
                        .get("platform")
                        .and_then(Value::as_str)
                        .is_none_or(|platform| platform.eq_ignore_ascii_case("openai"))
                        && account
                            .get("type")
                            .and_then(Value::as_str)
                            .is_none_or(|kind| kind.eq_ignore_ascii_case("oauth"))
                })
                .or_else(|| accounts.first())
        })
        .unwrap_or(value)
}

fn write_codex_auth_json(codex_dir: &Path, material: &Value) -> Result<()> {
    let auth_path = codex_dir.join("auth.json");
    let mut auth = if auth_path.exists() {
        let text = fs::read_to_string(&auth_path)
            .map_err(|source| CompanionError::io(&auth_path, source))?;
        let auth = serde_json::from_str::<Value>(&text).map_err(|source| {
            CompanionError::InvalidConfig(format!(
                "无法安全更新 Codex auth.json：现有文件不是有效 JSON: {source}"
            ))
        })?;
        if !auth.is_object() {
            return Err(CompanionError::InvalidConfig(
                "无法安全更新 Codex auth.json：现有文件不是 JSON object".to_string(),
            ));
        }
        auth
    } else {
        Value::Object(Default::default())
    };
    merge_codex_auth(&mut auth, material)?;
    let text = serde_json::to_string_pretty(&auth).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex auth.json 失败: {source}"))
    })?;
    fs::write(&auth_path, format!("{text}\n"))
        .map_err(|source| CompanionError::io(&auth_path, source))
}

fn merge_codex_auth(target: &mut Value, source: &Value) -> Result<()> {
    let Some(target_object) = target.as_object_mut() else {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 不是 JSON object".to_string(),
        ));
    };
    let Some(source_object) = source.as_object() else {
        return Err(CompanionError::InvalidConfig(
            "provider auth 不是 JSON object".to_string(),
        ));
    };
    for (key, value) in source_object {
        if key == "tokens" && value.is_object() {
            let target_tokens = target_object
                .entry("tokens".to_string())
                .or_insert_with(|| Value::Object(Default::default()));
            if !target_tokens.is_object() {
                *target_tokens = Value::Object(Default::default());
            }
            if let (Some(target_tokens), Some(source_tokens)) =
                (target_tokens.as_object_mut(), value.as_object())
            {
                for (token_key, token_value) in source_tokens {
                    target_tokens.insert(token_key.clone(), token_value.clone());
                }
            }
        } else {
            target_object.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn write_codex_openai_api_key(codex_dir: &Path, api_key: &str) -> Result<()> {
    let mut auth = serde_json::Map::new();
    auth.insert(
        "auth_mode".to_string(),
        Value::String(CODEX_API_KEY_AUTH_MODE.to_string()),
    );
    auth.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(api_key.to_string()),
    );
    write_codex_auth_json(codex_dir, &Value::Object(auth))
}

#[cfg(all(target_os = "macos", not(test)))]
fn write_codex_keychain_to_dir(codex_dir: &Path, material: &Value) -> Result<()> {
    use sha2::{Digest, Sha256};

    let resolved_home = fs::canonicalize(codex_dir).unwrap_or_else(|_| codex_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(resolved_home.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let digest_hex = format!("{digest:x}");
    let account = format!("cli|{}", &digest_hex[..16]);
    let secret = serde_json::to_string(material).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex Keychain 数据失败: {source}"))
    })?;
    let output = std::process::Command::new("security")
        .arg("add-generic-password")
        .arg("-U")
        .arg("-s")
        .arg(CODEX_KEYCHAIN_SERVICE)
        .arg("-a")
        .arg(account)
        .arg("-w")
        .arg(secret)
        .output()
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("执行 security 写入 Keychain 失败: {source}"))
        })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(CompanionError::InvalidConfig(format!(
        "写入 Codex Keychain 失败: {}",
        stderr.trim()
    )))
}

#[cfg(any(not(target_os = "macos"), test))]
fn write_codex_keychain_to_dir(_codex_dir: &Path, _material: &Value) -> Result<()> {
    Ok(())
}

fn pick_first_json_string(sources: &[&Value], paths: &[&[&str]]) -> Option<String> {
    sources
        .iter()
        .find_map(|source| pick_json_string(source, paths))
}

fn insert_optional_json_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value));
    }
}

fn pick_json_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
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
                .filter(|text| !text.is_empty())
            {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompanionInstallKind {
    Relay,
    Direct,
}

impl CompanionInstallKind {
    fn as_str(self) -> &'static str {
        match self {
            CompanionInstallKind::Relay => "relay",
            CompanionInstallKind::Direct => "direct",
        }
    }
}

#[derive(Debug, Clone)]
struct AuthRollback {
    bytes: Option<Vec<u8>>,
}

impl AuthRollback {
    fn capture(codex_dir: &Path) -> Result<Self> {
        let auth_path = codex_dir.join("auth.json");
        let bytes = if auth_path.exists() {
            Some(fs::read(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))?)
        } else {
            None
        };
        Ok(Self { bytes })
    }

    fn restore(&self, codex_dir: &Path) -> Result<()> {
        let auth_path = codex_dir.join("auth.json");
        match self.bytes.as_deref() {
            Some(bytes) => fs::write(&auth_path, bytes)
                .map_err(|source| CompanionError::io(&auth_path, source)),
            None if auth_path.exists() => {
                fs::remove_file(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))
            }
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone)]
struct ConfigRollback {
    bytes: Option<Vec<u8>>,
}

impl ConfigRollback {
    fn capture(config_path: &Path) -> Result<Self> {
        let bytes = if config_path.exists() {
            Some(fs::read(config_path).map_err(|source| CompanionError::io(config_path, source))?)
        } else {
            None
        };
        Ok(Self { bytes })
    }

    fn restore(&self, config_path: &Path) -> Result<()> {
        match self.bytes.as_deref() {
            Some(bytes) => fs::write(config_path, bytes)
                .map_err(|source| CompanionError::io(config_path, source)),
            None if config_path.exists() => fs::remove_file(config_path)
                .map_err(|source| CompanionError::io(config_path, source)),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ManagedConfigBackup {
    backup_root: String,
    config_backup: Option<String>,
    auth_backup: Option<String>,
    previous_config_exists: bool,
    previous_auth_exists: bool,
    previous_model_provider: Option<String>,
    auth_write_hash: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CompanionConfigMarker {
    backup_root: Option<String>,
    config_backup: Option<String>,
    auth_backup: Option<String>,
    previous_config_exists: bool,
    previous_auth_exists: bool,
    previous_model_provider: Option<String>,
    target_provider: Option<String>,
    install_kind: Option<String>,
    token_source: Option<String>,
    config_hash: Option<String>,
    auth_write_hash: Option<String>,
}

impl CompanionConfigMarker {
    fn from_doc(doc: &DocumentMut) -> Option<Self> {
        let marker = doc.get(COMPANION_MARKER_TABLE)?;
        if !marker
            .get("managed")
            .and_then(Item::as_bool)
            .unwrap_or(false)
        {
            return None;
        }
        Some(Self {
            backup_root: marker
                .get("backup_root")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            config_backup: marker
                .get("config_backup")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            auth_backup: marker
                .get("auth_backup")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            previous_config_exists: marker
                .get("previous_config_exists")
                .and_then(Item::as_bool)
                .unwrap_or(true),
            previous_auth_exists: marker
                .get("previous_auth_exists")
                .and_then(Item::as_bool)
                .unwrap_or(true),
            previous_model_provider: marker
                .get("previous_model_provider")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            target_provider: marker
                .get("target_provider")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            install_kind: marker
                .get("install_kind")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            token_source: marker
                .get("token_source")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            config_hash: marker
                .get("config_hash")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
            auth_write_hash: marker
                .get("auth_write_hash")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

fn prepare_config_write(
    codex_dir: &Path,
    config_path: &Path,
    doc: &DocumentMut,
) -> Result<ManagedConfigBackup> {
    if let Some(marker) = CompanionConfigMarker::from_doc(doc) {
        if !marker_hash_matches(doc, &marker) {
            snapshot_changed_config(codex_dir, config_path)?;
        }
        if let Some(backup_root) = marker.backup_root {
            return Ok(ManagedConfigBackup {
                backup_root,
                config_backup: marker.config_backup,
                auth_backup: marker.auth_backup,
                previous_config_exists: marker.previous_config_exists,
                previous_auth_exists: marker.previous_auth_exists,
                previous_model_provider: marker.previous_model_provider,
                auth_write_hash: marker.auth_write_hash,
            });
        }
    }

    let backup_root = create_backup_root(codex_dir)?;
    let config_backup = if config_path.exists() {
        backup_file_to_root(config_path, &backup_root, codex_dir)?
    } else {
        None
    };
    Ok(ManagedConfigBackup {
        backup_root: path_relative_to_codex_dir(&backup_root, codex_dir),
        config_backup,
        auth_backup: None,
        previous_config_exists: config_path.exists(),
        previous_auth_exists: false,
        previous_model_provider: doc
            .get("model_provider")
            .and_then(Item::as_str)
            .map(ToOwned::to_owned),
        auth_write_hash: None,
    })
}

fn ensure_auth_backup(backup: &mut ManagedConfigBackup, codex_dir: &Path) -> Result<()> {
    if backup.auth_backup.is_some() {
        return Ok(());
    }
    if backup.auth_write_hash.is_some() {
        ensure_managed_auth_write_unchanged(backup, codex_dir)?;
        return Ok(());
    }
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        backup.previous_auth_exists = false;
        return Ok(());
    }
    backup.previous_auth_exists = true;
    let backup_root = codex_dir.join(&backup.backup_root);
    backup.auth_backup = backup_file_to_root(&auth_path, &backup_root, codex_dir)?;
    Ok(())
}

fn record_auth_write(backup: &mut ManagedConfigBackup, codex_dir: &Path) -> Result<()> {
    let auth_path = codex_dir.join("auth.json");
    backup.auth_write_hash = Some(hash_file(&auth_path)?);
    Ok(())
}

fn ensure_managed_auth_write_unchanged(
    backup: &ManagedConfigBackup,
    codex_dir: &Path,
) -> Result<()> {
    let Some(expected_hash) = backup.auth_write_hash.as_deref() else {
        return Ok(());
    };
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后已不存在；为避免误覆盖账号材料，已停止写入"
                .to_string(),
        ));
    }
    let current_hash = hash_file(&auth_path)?;
    if current_hash != expected_hash {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后发生过修改；为避免覆盖官方登录或用户 API key，已停止写入"
                .to_string(),
        ));
    }
    Ok(())
}

fn restore_prior_auth_write_if_managed(
    backup: &mut ManagedConfigBackup,
    codex_dir: &Path,
) -> Result<bool> {
    if backup.auth_write_hash.is_none() {
        return Ok(false);
    }
    ensure_managed_auth_write_unchanged(backup, codex_dir)?;
    let auth_path = codex_dir.join("auth.json");
    if backup.previous_auth_exists {
        let backup_path = backup.auth_backup.as_deref().ok_or_else(|| {
            CompanionError::InvalidConfig(
                "Companion marker 表示原 auth.json 存在，但缺少 auth_backup；已停止写入"
                    .to_string(),
            )
        })?;
        let backup_path = resolve_codex_relative(codex_dir, backup_path);
        fs::copy(&backup_path, &auth_path)
            .map_err(|source| CompanionError::io(&backup_path, source))?;
    } else if auth_path.exists() {
        fs::remove_file(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))?;
    }
    backup.auth_backup = None;
    backup.previous_auth_exists = auth_path.exists();
    backup.auth_write_hash = None;
    Ok(true)
}

fn apply_companion_marker(
    doc: &mut DocumentMut,
    backup: &ManagedConfigBackup,
    install_kind: CompanionInstallKind,
    target_provider: &str,
    token_source: &str,
) {
    doc[COMPANION_MARKER_TABLE]["managed"] = value(true);
    doc[COMPANION_MARKER_TABLE]["version"] = value(COMPANION_MARKER_VERSION);
    doc[COMPANION_MARKER_TABLE]["install_kind"] = value(install_kind.as_str());
    doc[COMPANION_MARKER_TABLE]["target_provider"] = value(target_provider);
    doc[COMPANION_MARKER_TABLE]["backup_root"] = value(&backup.backup_root);
    doc[COMPANION_MARKER_TABLE]["previous_config_exists"] = value(backup.previous_config_exists);
    doc[COMPANION_MARKER_TABLE]["previous_auth_exists"] = value(backup.previous_auth_exists);
    if let Some(provider) = backup.previous_model_provider.as_deref() {
        doc[COMPANION_MARKER_TABLE]["previous_model_provider"] = value(provider);
    } else {
        doc[COMPANION_MARKER_TABLE]["previous_model_provider"] = Item::None;
    }
    if let Some(config_backup) = backup.config_backup.as_deref() {
        doc[COMPANION_MARKER_TABLE]["config_backup"] = value(config_backup);
    } else {
        doc[COMPANION_MARKER_TABLE]["config_backup"] = Item::None;
    }
    if let Some(auth_backup) = backup.auth_backup.as_deref() {
        doc[COMPANION_MARKER_TABLE]["auth_backup"] = value(auth_backup);
    } else {
        doc[COMPANION_MARKER_TABLE]["auth_backup"] = Item::None;
    }
    if let Some(auth_hash) = backup.auth_write_hash.as_deref() {
        doc[COMPANION_MARKER_TABLE]["auth_write_hash"] = value(auth_hash);
    } else {
        doc[COMPANION_MARKER_TABLE]["auth_write_hash"] = Item::None;
    }
    doc[COMPANION_MARKER_TABLE]["token_source"] = value(token_source);
    doc[COMPANION_MARKER_TABLE]["written_at"] = value(Local::now().to_rfc3339());
    doc[COMPANION_MARKER_TABLE]["config_hash"] = value("");
    let hash = config_doc_hash(doc);
    doc[COMPANION_MARKER_TABLE]["config_hash"] = value(hash);
}

fn marker_hash_matches(doc: &DocumentMut, marker: &CompanionConfigMarker) -> bool {
    let Some(expected) = marker
        .config_hash
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    config_doc_hash(doc) == expected
}

fn snapshot_changed_config(codex_dir: &Path, config_path: &Path) -> Result<()> {
    let backup_root = create_backup_root(codex_dir)?;
    backup_file(config_path, &backup_root, codex_dir)
}

fn config_doc_hash(doc: &DocumentMut) -> String {
    let mut clone = doc.clone();
    if clone.get(COMPANION_MARKER_TABLE).is_some() {
        clone[COMPANION_MARKER_TABLE]["config_hash"] = value("");
    }
    stable_hash_hex(&clone.to_string())
}

fn stable_hash_hex(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|source| CompanionError::io(path, source))?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in &bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("{hash:016x}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexAuthShape {
    Missing,
    Empty,
    OfficialOAuth,
    ApiKey,
    ApiKeyAndOAuth,
}

impl CodexAuthShape {
    fn label(self) -> &'static str {
        match self {
            CodexAuthShape::Missing => "missing auth.json",
            CodexAuthShape::Empty => "empty auth.json",
            CodexAuthShape::OfficialOAuth => "official OAuth tokens",
            CodexAuthShape::ApiKey => "OPENAI_API_KEY",
            CodexAuthShape::ApiKeyAndOAuth => "OPENAI_API_KEY plus official OAuth tokens",
        }
    }

    fn relay_token_source(self) -> &'static str {
        match self {
            CodexAuthShape::OfficialOAuth | CodexAuthShape::ApiKeyAndOAuth => {
                "Companion relay injection from official OAuth/auth material"
            }
            CodexAuthShape::ApiKey => "Companion relay injection from API key auth material",
            CodexAuthShape::Missing | CodexAuthShape::Empty => {
                "Companion relay injection from selected provider"
            }
        }
    }

    fn has_official_oauth(self) -> bool {
        matches!(
            self,
            CodexAuthShape::OfficialOAuth | CodexAuthShape::ApiKeyAndOAuth
        )
    }
}

fn detect_codex_auth_shape(codex_dir: &Path) -> Result<CodexAuthShape> {
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        return Ok(CodexAuthShape::Missing);
    }
    let text =
        fs::read_to_string(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))?;
    let auth = serde_json::from_str::<Value>(&text).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "无法识别 Codex auth.json 风险：现有文件不是有效 JSON: {source}"
        ))
    })?;
    if !auth.is_object() {
        return Err(CompanionError::InvalidConfig(
            "无法识别 Codex auth.json 风险：现有文件不是 JSON object".to_string(),
        ));
    }
    Ok(classify_codex_auth_shape(&auth))
}

fn classify_codex_auth_shape(auth: &Value) -> CodexAuthShape {
    let has_api_key = auth
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let null = Value::Null;
    let candidate = oauth_account_candidate(auth);
    let tokens = candidate
        .get("tokens")
        .or_else(|| auth.get("tokens"))
        .unwrap_or(&null);
    let credentials = candidate
        .get("credentials")
        .or_else(|| auth.get("credentials"))
        .unwrap_or(&null);
    let auth_source = auth.get("auth").unwrap_or(&null);
    let sources = [tokens, credentials, auth_source, candidate, auth];
    let has_oauth = pick_first_json_string(
        &sources,
        &[
            &["refresh_token"],
            &["refreshToken"],
            &["access_token"],
            &["accessToken"],
            &["id_token"],
            &["idToken"],
            &["session_token"],
            &["sessionToken"],
            &["chatgpt_account_id"],
            &["account_id"],
            &["accountId"],
            &["credentials", "refresh_token"],
            &["credentials", "access_token"],
            &["tokens", "refresh_token"],
            &["tokens", "access_token"],
        ],
    )
    .is_some();
    match (has_api_key, has_oauth) {
        (true, true) => CodexAuthShape::ApiKeyAndOAuth,
        (true, false) => CodexAuthShape::ApiKey,
        (false, true) => CodexAuthShape::OfficialOAuth,
        (false, false) => CodexAuthShape::Empty,
    }
}

fn direct_auth_warning(
    before: &CodexAuthShape,
    after: &CodexAuthShape,
    token_source: &str,
) -> String {
    let mut parts = vec![format!(
        "；auth.json: {} -> {}",
        before.label(),
        after.label()
    )];
    if before.has_official_oauth() && token_source.contains("API key") {
        parts.push(
            "；warning: direct API key mode updates OPENAI_API_KEY while preserving existing OAuth refresh data"
                .to_string(),
        );
    }
    parts.concat()
}

pub fn uninstall_companion_provider(codex_dir: Option<PathBuf>) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    let config_path = codex_dir.join("config.toml");
    if !config_path.exists() {
        return doctor(codex_dir, &RelayConfig::default());
    }
    let current = fs::read_to_string(&config_path)
        .map_err(|source| CompanionError::io(&config_path, source))?;
    let doc = current.parse::<DocumentMut>().map_err(|source| {
        CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
    })?;
    let marker = match CompanionConfigMarker::from_doc(&doc) {
        Some(marker) => marker,
        None => {
            return uninstall_legacy_companion_provider(codex_dir, config_path, doc);
        }
    };
    let live_provider = doc.get("model_provider").and_then(Item::as_str);
    let marker_metadata_matches =
        matches!(marker.install_kind.as_deref(), Some("relay" | "direct"))
            && marker.target_provider.as_deref() == live_provider;
    let config_changed = !marker_hash_matches(&doc, &marker) || !marker_metadata_matches;
    if config_changed {
        snapshot_changed_config(&codex_dir, &config_path)?;
    }
    validate_restore_inputs(&codex_dir, &marker)?;
    restore_managed_install(&codex_dir, &config_path, &marker, config_changed, doc)?;
    doctor(codex_dir, &RelayConfig::default())
}

fn uninstall_legacy_companion_provider(
    codex_dir: PathBuf,
    config_path: PathBuf,
    mut doc: DocumentMut,
) -> Result<CodexInstallStatus> {
    let live_provider = doc.get("model_provider").and_then(Item::as_str);
    if live_provider != Some(COMPANION_PROVIDER_ID) {
        return Err(CompanionError::InvalidConfig(
            "Codex config.toml 没有 Companion ownership marker；为避免覆盖用户配置，已停止卸载"
                .to_string(),
        ));
    }
    let has_companion_provider = doc
        .get("model_providers")
        .and_then(|item| item.get(COMPANION_PROVIDER_ID))
        .is_some();
    if !has_companion_provider {
        return Err(CompanionError::InvalidConfig(
            "Codex config.toml 当前 provider 是 Companion，但缺少 Companion provider 配置；为避免覆盖用户配置，已停止卸载"
                .to_string(),
        ));
    }

    doc["model_provider"] = value("openai");
    doc["model_providers"][COMPANION_PROVIDER_ID] = Item::None;
    fs::write(&config_path, doc.to_string())
        .map_err(|source| CompanionError::io(&config_path, source))?;
    doctor(codex_dir, &RelayConfig::default())
}

fn validate_restore_inputs(codex_dir: &Path, marker: &CompanionConfigMarker) -> Result<()> {
    if marker.previous_config_exists {
        let backup = marker.config_backup.as_deref().ok_or_else(|| {
            CompanionError::InvalidConfig(
                "Companion marker 表示原 config.toml 存在，但缺少 config_backup；已停止卸载"
                    .to_string(),
            )
        })?;
        let backup_path = resolve_codex_relative(codex_dir, backup);
        if !backup_path.exists() {
            return Err(CompanionError::InvalidConfig(format!(
                "Companion config backup 不存在：{}；已停止卸载且未恢复 auth.json",
                backup_path.display()
            )));
        }
    }
    let Some(expected_hash) = marker.auth_write_hash.as_deref() else {
        return Ok(());
    };
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后已不存在；为避免误恢复账号材料，已停止卸载"
                .to_string(),
        ));
    }
    let current_hash = hash_file(&auth_path)?;
    if current_hash != expected_hash {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后发生过修改；为避免覆盖官方登录或用户 API key，已停止卸载"
                .to_string(),
        ));
    }
    if marker.previous_auth_exists {
        let backup = marker.auth_backup.as_deref().ok_or_else(|| {
            CompanionError::InvalidConfig(
                "Companion marker 表示原 auth.json 存在，但缺少 auth_backup；已停止卸载"
                    .to_string(),
            )
        })?;
        let backup_path = resolve_codex_relative(codex_dir, backup);
        if !backup_path.exists() {
            return Err(CompanionError::InvalidConfig(format!(
                "Companion auth backup 不存在：{}；已停止卸载且未恢复 config.toml",
                backup_path.display()
            )));
        }
    }
    Ok(())
}

fn restore_managed_install(
    codex_dir: &Path,
    config_path: &Path,
    marker: &CompanionConfigMarker,
    config_changed: bool,
    current_doc: DocumentMut,
) -> Result<()> {
    let config_rollback = ConfigRollback::capture(config_path)?;
    let auth_rollback = AuthRollback::capture(codex_dir)?;
    if config_changed {
        restore_changed_config_from_marker(codex_dir, config_path, marker, current_doc)?;
    } else {
        restore_config_from_marker(codex_dir, config_path, marker)?;
    }
    if let Err(auth_error) = restore_auth_from_marker(codex_dir, marker) {
        let config_restore = config_rollback.restore(config_path);
        let auth_restore = auth_rollback.restore(codex_dir);
        if let Err(rollback_error) = config_restore.and(auth_restore) {
            return Err(CompanionError::InvalidConfig(format!(
                "卸载已恢复 config.toml 但恢复 auth.json 失败: {auth_error}；尝试回滚卸载状态也失败: {rollback_error}"
            )));
        }
        return Err(auth_error);
    }
    Ok(())
}

fn restore_changed_config_from_marker(
    codex_dir: &Path,
    config_path: &Path,
    marker: &CompanionConfigMarker,
    mut current_doc: DocumentMut,
) -> Result<()> {
    let original_doc = marker
        .config_backup
        .as_deref()
        .map(|backup| resolve_codex_relative(codex_dir, backup))
        .map(|backup_path| {
            let text = fs::read_to_string(&backup_path)
                .map_err(|source| CompanionError::io(&backup_path, source))?;
            text.parse::<DocumentMut>().map_err(|source| {
                CompanionError::InvalidConfig(format!(
                    "Companion config backup 不是有效 TOML: {source}"
                ))
            })
        })
        .transpose()?;

    let live_provider = current_doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(ToOwned::to_owned);
    if live_provider.as_deref() == marker.target_provider.as_deref() {
        if let Some(original_provider) = original_doc
            .as_ref()
            .and_then(|doc| doc.get("model_provider"))
        {
            current_doc["model_provider"] = original_provider.clone();
        } else if let Some(previous_provider) = marker.previous_model_provider.as_deref() {
            current_doc["model_provider"] = value(previous_provider);
        } else {
            current_doc["model_provider"] = Item::None;
        }
    }

    if let Some(target_provider) = marker.target_provider.as_deref() {
        restore_model_provider_entry(&mut current_doc, original_doc.as_ref(), target_provider);
    }
    if let Some(previous_provider) = marker.previous_model_provider.as_deref() {
        restore_missing_model_provider_entry(
            &mut current_doc,
            original_doc.as_ref(),
            previous_provider,
        );
    }
    if current_doc.get("openai_base_url").is_none() {
        if let Some(original_base_url) = original_doc
            .as_ref()
            .and_then(|doc| doc.get("openai_base_url"))
        {
            current_doc["openai_base_url"] = original_base_url.clone();
        }
    }
    current_doc.as_table_mut().remove(COMPANION_MARKER_TABLE);
    fs::write(config_path, current_doc.to_string())
        .map_err(|source| CompanionError::io(config_path, source))
}

fn restore_model_provider_entry(
    current_doc: &mut DocumentMut,
    original_doc: Option<&DocumentMut>,
    provider_id: &str,
) {
    let original = original_doc
        .and_then(|doc| doc.get("model_providers"))
        .and_then(|providers| providers.get(provider_id))
        .cloned();
    if let Some(original) = original {
        current_doc["model_providers"][provider_id] = original;
        return;
    }
    if let Some(providers) = current_doc
        .get_mut("model_providers")
        .and_then(Item::as_table_like_mut)
    {
        providers.remove(provider_id);
        if providers.is_empty() {
            current_doc["model_providers"] = Item::None;
        }
    }
}

fn restore_missing_model_provider_entry(
    current_doc: &mut DocumentMut,
    original_doc: Option<&DocumentMut>,
    provider_id: &str,
) {
    let already_present = current_doc
        .get("model_providers")
        .and_then(|providers| providers.get(provider_id))
        .is_some();
    if already_present {
        return;
    }
    if let Some(original) = original_doc
        .and_then(|doc| doc.get("model_providers"))
        .and_then(|providers| providers.get(provider_id))
        .cloned()
    {
        current_doc["model_providers"][provider_id] = original;
    }
}

fn restore_config_from_marker(
    codex_dir: &Path,
    config_path: &Path,
    marker: &CompanionConfigMarker,
) -> Result<()> {
    if marker.previous_config_exists {
        let backup = marker.config_backup.as_deref().ok_or_else(|| {
            CompanionError::InvalidConfig(
                "Companion marker 表示原 config.toml 存在，但缺少 config_backup；已停止卸载"
                    .to_string(),
            )
        })?;
        let backup_path = resolve_codex_relative(codex_dir, backup);
        fs::copy(&backup_path, config_path)
            .map_err(|source| CompanionError::io(&backup_path, source))?;
    } else if config_path.exists() {
        fs::remove_file(config_path).map_err(|source| CompanionError::io(config_path, source))?;
    }
    Ok(())
}

fn restore_auth_from_marker(codex_dir: &Path, marker: &CompanionConfigMarker) -> Result<()> {
    let Some(expected_hash) = marker.auth_write_hash.as_deref() else {
        return Ok(());
    };
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后已不存在；为避免误恢复账号材料，已停止卸载"
                .to_string(),
        ));
    }
    let current_hash = hash_file(&auth_path)?;
    if current_hash != expected_hash {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 在 Companion 写入后发生过修改；为避免覆盖官方登录或用户 API key，已停止卸载"
                .to_string(),
        ));
    }
    if marker.previous_auth_exists {
        let backup = marker.auth_backup.as_deref().ok_or_else(|| {
            CompanionError::InvalidConfig(
                "Companion marker 表示原 auth.json 存在，但缺少 auth_backup；已停止卸载"
                    .to_string(),
            )
        })?;
        let backup_path = resolve_codex_relative(codex_dir, backup);
        fs::copy(&backup_path, &auth_path)
            .map_err(|source| CompanionError::io(&backup_path, source))?;
    } else {
        fs::remove_file(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))?;
    }
    Ok(())
}

pub fn doctor(codex_dir: PathBuf, relay: &RelayConfig) -> Result<CodexInstallStatus> {
    let config_path = codex_dir.join("config.toml");
    let mut model_provider = None;
    let mut installed = false;
    let mut token_source = None;
    if config_path.exists() {
        let current = fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?;
        if let Ok(doc) = current.parse::<DocumentMut>() {
            token_source = CompanionConfigMarker::from_doc(&doc).and_then(|marker| {
                let live_provider = doc.get("model_provider").and_then(Item::as_str);
                if marker.target_provider.as_deref() == live_provider {
                    marker.token_source
                } else {
                    None
                }
            });
            model_provider = doc
                .get("model_provider")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned);
            installed = model_provider.as_deref() == Some(COMPANION_PROVIDER_ID)
                && doc
                    .get("model_providers")
                    .and_then(|item| item.get(COMPANION_PROVIDER_ID))
                    .and_then(|item| item.get("base_url"))
                    .and_then(Item::as_str)
                    .is_some_and(|value| value == relay.base_url());
        }
    }
    let message = if installed {
        "Codex 已配置为使用本地代理".to_string()
    } else if let Some(provider) = model_provider.as_deref() {
        format!("Codex 当前配置 provider: {provider}")
    } else if config_path.exists() {
        "Codex 配置存在，但尚未设置 model_provider".to_string()
    } else {
        "Codex 配置尚未创建，可在设置里写入本地代理配置".to_string()
    };
    let message = if let Some(token_source) = token_source.as_deref() {
        format!("{message}；Token source: {token_source}")
    } else {
        message
    };
    Ok(CodexInstallStatus {
        codex_dir,
        config_path,
        installed,
        model_provider,
        companion_base_url: relay.base_url(),
        message,
    })
}

pub fn repair_state(options: RepairOptions) -> Result<RepairOutcome> {
    if !options.history && !options.plugins {
        return Err(CompanionError::InvalidConfig(
            "修复需要指定 --history、--plugins 或同时指定两者".to_string(),
        ));
    }
    if !options.codex_dir.exists() {
        return Err(CompanionError::InvalidConfig(format!(
            "Codex 目录不存在: {}",
            options.codex_dir.display()
        )));
    }
    let target_provider_id = resolve_repair_target_provider_id(
        &options.codex_dir,
        options.target_provider_id.as_deref(),
    )?;

    let jsonl_files = if options.history {
        collect_history_jsonl_files(&options.codex_dir)
    } else {
        Vec::new()
    };
    let plugin_files = if options.plugins {
        collect_plugin_json_files(&options.codex_dir)
    } else {
        Vec::new()
    };
    let db_path = options.codex_dir.join(CODEX_STATE_DB_FILENAME);

    let mut source_provider_ids = collect_jsonl_provider_ids(&jsonl_files)?;
    if options.history && db_path.exists() {
        source_provider_ids.extend(collect_sqlite_provider_ids(&db_path)?);
    }
    if options.plugins {
        source_provider_ids.extend(collect_plugin_provider_ids(&plugin_files)?);
    }
    source_provider_ids.remove(target_provider_id.as_str());

    let session_metadata = if options.history {
        collect_session_metadata(&jsonl_files)?
    } else {
        Default::default()
    };
    let state_rows = if options.history && db_path.exists() {
        count_sqlite_rows_to_repair(
            &db_path,
            &source_provider_ids,
            &target_provider_id,
            &session_metadata,
        )?
    } else {
        0
    };
    let history_lines = if options.history {
        count_jsonl_lines_to_migrate(&jsonl_files, &source_provider_ids)?
    } else {
        0
    };

    let plan = RepairPlan {
        codex_dir: options.codex_dir.clone(),
        target_provider_id: target_provider_id.clone(),
        history_files: jsonl_files.len(),
        history_lines,
        plugin_files: plugin_files.len(),
        state_rows,
        source_provider_ids: source_provider_ids.iter().cloned().collect(),
        dry_run: options.dry_run,
    };

    if source_provider_ids.is_empty() && state_rows == 0 {
        return Ok(RepairOutcome {
            plan,
            backup_root: None,
            migrated_history_files: 0,
            migrated_history_lines: 0,
            migrated_plugin_files: 0,
            migrated_state_rows: 0,
            skipped_reason: Some(format!(
                "未发现需要迁移到 {target_provider_id} 的 provider namespace"
            )),
        });
    }

    if options.dry_run {
        return Ok(RepairOutcome {
            plan,
            backup_root: None,
            migrated_history_files: 0,
            migrated_history_lines: 0,
            migrated_plugin_files: 0,
            migrated_state_rows: 0,
            skipped_reason: None,
        });
    }

    let (
        backup_root,
        (
            migrated_history_files,
            migrated_history_lines,
            migrated_plugin_files,
            migrated_state_rows,
        ),
    ) = run_repair_transaction(&options.codex_dir, |backup_root| {
        let mut migrated_history_files = 0;
        let mut migrated_history_lines = 0;
        let mut migrated_plugin_files = 0;
        let mut migrated_state_rows = 0;

        if options.history && history_lines > 0 {
            let backup_root = ensure_repair_backup_root(backup_root, &options.codex_dir)?;
            let history_migration = rewrite_history_files(
                &jsonl_files,
                &source_provider_ids,
                &target_provider_id,
                backup_root,
                &options.codex_dir,
            )?;
            migrated_history_files = history_migration.files;
            migrated_history_lines = history_migration.lines;
        }

        if options.history && db_path.exists() && state_rows > 0 {
            let backup_root = ensure_repair_backup_root(backup_root, &options.codex_dir)?;
            backup_file(&db_path, backup_root, &options.codex_dir)?;
            migrated_state_rows = repair_sqlite_threads(
                &db_path,
                &source_provider_ids,
                &target_provider_id,
                &session_metadata,
            )?;
        }

        for path in &plugin_files {
            if rewrite_plugin_file(
                path,
                &source_provider_ids,
                &target_provider_id,
                backup_root,
                &options.codex_dir,
            )? {
                migrated_plugin_files += 1;
            }
        }

        Ok((
            migrated_history_files,
            migrated_history_lines,
            migrated_plugin_files,
            migrated_state_rows,
        ))
    })?;

    let skipped_reason =
        if migrated_history_files == 0 && migrated_plugin_files == 0 && migrated_state_rows == 0 {
            Some(format!(
                "未发现需要迁移到 {target_provider_id} 的可合并状态"
            ))
        } else {
            None
        };

    Ok(RepairOutcome {
        plan,
        backup_root,
        migrated_history_files,
        migrated_history_lines,
        migrated_plugin_files,
        migrated_state_rows,
        skipped_reason,
    })
}

fn resolve_repair_target_provider_id(codex_dir: &Path, requested: Option<&str>) -> Result<String> {
    if let Some(provider_id) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(provider_id.to_string());
    }

    let config_path = codex_dir.join("config.toml");
    if config_path.exists() {
        let current = fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?;
        let doc = current.parse::<DocumentMut>().map_err(|source| {
            CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
        })?;
        if let Some(provider_id) = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(provider_id.to_string());
        }
    }

    if codex_auth_json_looks_like_official_account(codex_dir) {
        return Ok(CODEX_OPENAI_PROVIDER_ID.to_string());
    }

    Ok(COMPANION_PROVIDER_ID.to_string())
}

fn collect_history_jsonl_files(root: &Path) -> Vec<PathBuf> {
    ["sessions", "archived_sessions"]
        .iter()
        .map(|dir| root.join(dir))
        .filter(|dir| dir.exists())
        .flat_map(|dir| collect_files(&dir, "jsonl"))
        .collect()
}

fn collect_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_repair_backup_path(entry.path()))
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|value| value == extension))
        .collect()
}

fn codex_auth_json_looks_like_official_account(codex_dir: &Path) -> bool {
    let auth_path = codex_dir.join("auth.json");
    let Ok(text) = fs::read_to_string(&auth_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let auth_mode = pick_json_string(&value, &[&["auth_mode"], &["authMode"]]);
    if auth_mode
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(CODEX_API_KEY_AUTH_MODE))
    {
        return false;
    }
    pick_json_string(
        &value,
        &[
            &["tokens", "access_token"],
            &["credentials", "access_token"],
            &["access_token"],
            &["token"],
        ],
    )
    .is_some()
}

fn is_repair_backup_path(path: &Path) -> bool {
    let parts = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    parts
        .windows(2)
        .any(|window| window[0] == "backups" && window[1] == "codex-companion")
}

fn collect_plugin_json_files(root: &Path) -> Vec<PathBuf> {
    let known_dirs = [
        "plugins",
        "plugin-state",
        "plugin_state",
        "mcp",
        "mcp_state",
    ];
    known_dirs
        .iter()
        .map(|dir| root.join(dir))
        .filter(|dir| dir.exists())
        .flat_map(|dir| collect_files(&dir, "json"))
        .collect()
}

fn collect_plugin_provider_ids(files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            collect_provider_fields(&value, &mut ids);
        }
    }
    Ok(ids)
}

fn collect_provider_fields(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if is_provider_field_key(key) {
                    if let Some(current) = child.as_str().filter(|value| !value.is_empty()) {
                        ids.insert(current.to_string());
                        continue;
                    }
                }
                collect_provider_fields(child, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_provider_fields(item, ids);
            }
        }
        _ => {}
    }
}

fn collect_jsonl_provider_ids(files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            if let Some(provider) = session_meta_provider(&line) {
                ids.insert(provider);
            }
        }
    }
    Ok(ids)
}

fn collect_session_metadata(files: &[PathBuf]) -> Result<BTreeMap<String, SessionMetadata>> {
    let mut metadata = BTreeMap::new();
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            let Some((id, mut entry)) = session_meta_metadata(&line) else {
                continue;
            };
            entry
                .rollout_path
                .get_or_insert_with(|| path.display().to_string());
            metadata
                .entry(id)
                .and_modify(|current: &mut SessionMetadata| current.merge_missing(&entry))
                .or_insert(entry);
        }
    }
    Ok(metadata)
}

impl SessionMetadata {
    fn merge_missing(&mut self, other: &SessionMetadata) {
        if self.cwd.as_deref().is_none_or(str::is_empty) {
            self.cwd = other.cwd.clone();
        }
        if self.rollout_path.as_deref().is_none_or(str::is_empty) {
            self.rollout_path = other.rollout_path.clone();
        }
    }
}

fn session_meta_provider(line: &str) -> Option<String> {
    if !line.contains("session_meta") || !line.contains("model_provider") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("payload")?
        .get("model_provider")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn session_meta_metadata(line: &str) -> Option<(String, SessionMetadata)> {
    if !line.contains("session_meta") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;
    let id = payload
        .get("id")?
        .as_str()
        .filter(|value| !value.is_empty())?
        .to_string();
    Some((
        id,
        SessionMetadata {
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            rollout_path: payload
                .get("rollout_path")
                .or_else(|| payload.get("rolloutPath"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        },
    ))
}

fn count_jsonl_lines_to_migrate(files: &[PathBuf], source_ids: &BTreeSet<String>) -> Result<usize> {
    if source_ids.is_empty() {
        return Ok(0);
    }
    let mut count = 0;
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            if session_meta_provider(&line).is_some_and(|provider| source_ids.contains(&provider)) {
                count += 1;
            }
        }
    }
    Ok(count)
}

#[derive(Debug, Default)]
struct HistoryMigration {
    files: usize,
    lines: usize,
}

fn rewrite_history_files(
    files: &[PathBuf],
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    backup_root: &Path,
    codex_dir: &Path,
) -> Result<HistoryMigration> {
    if source_ids.is_empty() {
        return Ok(HistoryMigration::default());
    }
    let mut total = HistoryMigration::default();
    for path in files {
        let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
        let trailing_newline = text.ends_with('\n');
        let mut changed_lines = 0;
        let mut next_lines = Vec::new();
        for line in text.lines() {
            if let Some(next_line) =
                rewrite_session_meta_provider_line(line, source_ids, target_provider_id)?
            {
                changed_lines += 1;
                next_lines.push(next_line);
            } else {
                next_lines.push(line.to_string());
            }
        }
        if changed_lines == 0 {
            continue;
        }

        backup_file(path, backup_root, codex_dir)?;
        let mut next = next_lines.join("\n");
        if trailing_newline {
            next.push('\n');
        }
        fs::write(path, next).map_err(|source| CompanionError::io(path, source))?;
        total.files += 1;
        total.lines += changed_lines;
    }
    Ok(total)
}

fn rewrite_session_meta_provider_line(
    line: &str,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
) -> Result<Option<String>> {
    if !line.contains("session_meta") || !line.contains("model_provider") {
        return Ok(None);
    }
    let mut value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(provider) = value
        .get("payload")
        .and_then(|payload| payload.get("model_provider"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    if !source_ids.contains(provider) {
        return Ok(None);
    }
    if let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) {
        payload.insert(
            "model_provider".to_string(),
            Value::String(target_provider_id.to_string()),
        );
    }
    let next = serde_json::to_string(&value).map_err(|source| {
        CompanionError::InvalidConfig(format!("JSONL rewrite failed: {source}"))
    })?;
    Ok(Some(next))
}

fn collect_sqlite_provider_ids(path: &Path) -> Result<BTreeSet<String>> {
    let conn = open_sqlite(path)?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT model_provider FROM threads WHERE model_provider IS NOT NULL")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let values = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let mut ids = BTreeSet::new();
    for value in values {
        let value = value.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite row failed: {source}"))
        })?;
        if !value.is_empty() {
            ids.insert(value);
        }
    }
    Ok(ids)
}

fn count_sqlite_rows_to_repair(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: &BTreeMap<String, SessionMetadata>,
) -> Result<usize> {
    let conn = open_sqlite(path)?;
    if !sqlite_threads_have_repair_columns(&conn)? {
        return Ok(0);
    }
    let mut stmt = conn
        .prepare("SELECT id, model_provider, cwd, rollout_path FROM threads")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SqliteThreadRow {
                id: row.get(0)?,
                model_provider: row.get(1)?,
                cwd: row.get(2)?,
                rollout_path: row.get(3)?,
            })
        })
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let mut total = 0;
    for row in rows {
        let row = row.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite row failed: {source}"))
        })?;
        if sqlite_thread_needs_repair(&row, source_ids, target_provider_id, metadata.get(&row.id)) {
            total += 1;
        }
    }
    Ok(total)
}

fn repair_sqlite_threads(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: &BTreeMap<String, SessionMetadata>,
) -> Result<usize> {
    let conn = open_sqlite(path)?;
    if !sqlite_threads_have_repair_columns(&conn)? {
        return Ok(0);
    }
    let mut total = 0;
    let mut stmt = conn
        .prepare("SELECT id, model_provider, cwd, rollout_path FROM threads")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SqliteThreadRow {
                id: row.get(0)?,
                model_provider: row.get(1)?,
                cwd: row.get(2)?,
                rollout_path: row.get(3)?,
            })
        })
        .map_err(|source| CompanionError::InvalidConfig(format!("SQLite query failed: {source}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| CompanionError::InvalidConfig(format!("SQLite row failed: {source}")))?;
    drop(stmt);

    for row in rows {
        let row_metadata = metadata.get(&row.id);
        if !sqlite_thread_needs_repair(&row, source_ids, target_provider_id, row_metadata) {
            continue;
        }
        let next_provider = if source_ids.contains(&row.model_provider) {
            target_provider_id.to_string()
        } else {
            row.model_provider.clone()
        };
        let next_cwd = merge_string_field(
            &row.cwd,
            row_metadata.and_then(|value| value.cwd.as_deref()),
        );
        let next_rollout_path = merge_string_field(
            &row.rollout_path,
            row_metadata.and_then(|value| value.rollout_path.as_deref()),
        );
        total += conn
            .execute(
                "UPDATE threads SET model_provider = ?, cwd = ?, rollout_path = ? WHERE id = ?",
                params![next_provider, next_cwd, next_rollout_path, row.id],
            )
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("SQLite update failed: {source}"))
            })?;
    }
    Ok(total)
}

#[derive(Debug)]
struct SqliteThreadRow {
    id: String,
    model_provider: String,
    cwd: String,
    rollout_path: String,
}

fn sqlite_thread_needs_repair(
    row: &SqliteThreadRow,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: Option<&SessionMetadata>,
) -> bool {
    if source_ids.contains(&row.model_provider) {
        return true;
    }
    if row.model_provider != target_provider_id {
        return false;
    }
    metadata.is_some_and(|metadata| {
        string_field_needs_merge(&row.cwd, metadata.cwd.as_deref())
            || string_field_needs_merge(&row.rollout_path, metadata.rollout_path.as_deref())
    })
}

fn string_field_needs_merge(current: &str, incoming: Option<&str>) -> bool {
    current.trim().is_empty() && incoming.is_some_and(|value| !value.trim().is_empty())
}

fn merge_string_field(current: &str, incoming: Option<&str>) -> String {
    if string_field_needs_merge(current, incoming) {
        incoming.unwrap_or_default().to_string()
    } else {
        current.to_string()
    }
}

fn sqlite_table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })?;
    for value in columns {
        if value.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })? == column
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_threads_have_repair_columns(conn: &Connection) -> Result<bool> {
    for column in ["id", "model_provider", "cwd", "rollout_path"] {
        if !sqlite_table_has_column(conn, "threads", column)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn open_sqlite(path: &Path) -> Result<Connection> {
    Connection::open(path).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "failed to open SQLite at {}: {source}",
            path.display()
        ))
    })
}

fn rewrite_plugin_file(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    backup_root: &mut Option<PathBuf>,
    codex_dir: &Path,
) -> Result<bool> {
    let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
    let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
        return Ok(false);
    };
    if rewrite_provider_fields(&mut value, source_ids, target_provider_id) {
        let backup_root = ensure_repair_backup_root(backup_root, codex_dir)?;
        backup_file(path, backup_root, codex_dir)?;
        let next = serde_json::to_string_pretty(&value)
            .map_err(|source| CompanionError::json(path, source))?;
        fs::write(path, format!("{next}\n")).map_err(|source| CompanionError::io(path, source))?;
        return Ok(true);
    }
    Ok(false)
}

fn run_repair_transaction<T>(
    codex_dir: &Path,
    mutate: impl FnOnce(&mut Option<PathBuf>) -> Result<T>,
) -> Result<(Option<PathBuf>, T)> {
    let mut backup_root = None;
    match mutate(&mut backup_root) {
        Ok(value) => {
            if backup_root.is_some() {
                let _ = cleanup_old_repair_backups(codex_dir, REPAIR_BACKUP_RETENTION);
            }
            Ok((backup_root, value))
        }
        Err(error) => {
            let Some(root) = backup_root.as_deref() else {
                return Err(error);
            };
            match restore_repair_backup(root, codex_dir) {
                Ok(()) => Err(CompanionError::InvalidConfig(format!(
                    "{error}；修复未完成，已从 {} 回滚已修改文件",
                    root.display()
                ))),
                Err(rollback_error) => Err(CompanionError::InvalidConfig(format!(
                    "{error}；自动回滚也失败: {rollback_error}；备份位于 {}",
                    root.display()
                ))),
            }
        }
    }
}

fn repair_backup_parent(codex_dir: &Path) -> PathBuf {
    codex_dir
        .join("backups")
        .join("codex-companion")
        .join("repair")
}

fn create_repair_backup_root(codex_dir: &Path) -> Result<PathBuf> {
    let parent = repair_backup_parent(codex_dir);
    fs::create_dir_all(&parent).map_err(|source| CompanionError::io(&parent, source))?;
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            timestamp.clone()
        } else {
            format!("{timestamp}-{suffix}")
        };
        let root = parent.join(name);
        match fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(CompanionError::io(&root, source)),
        }
    }
    Err(CompanionError::InvalidConfig(
        "无法创建唯一的会话修复备份目录".to_string(),
    ))
}

fn ensure_repair_backup_root<'a>(
    backup_root: &'a mut Option<PathBuf>,
    codex_dir: &Path,
) -> Result<&'a Path> {
    if backup_root.is_none() {
        *backup_root = Some(create_repair_backup_root(codex_dir)?);
    }
    Ok(backup_root.as_deref().expect("repair backup root"))
}

fn restore_repair_backup(backup_root: &Path, codex_dir: &Path) -> Result<()> {
    let mut files = WalkDir::new(backup_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    files.sort();
    for backup_path in files {
        let relative = backup_path.strip_prefix(backup_root).map_err(|source| {
            CompanionError::InvalidConfig(format!(
                "无法解析修复备份路径 {}: {source}",
                backup_path.display()
            ))
        })?;
        let target = codex_dir.join(relative);
        if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|source| CompanionError::io(&target, source))?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        fs::copy(&backup_path, &target)
            .map_err(|source| CompanionError::io(&backup_path, source))?;
    }
    Ok(())
}

fn cleanup_old_repair_backups(codex_dir: &Path, retain: usize) -> Result<()> {
    let parent = repair_backup_parent(codex_dir);
    let Ok(entries) = fs::read_dir(&parent) else {
        return Ok(());
    };
    let mut directories = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_dir())
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    directories.sort();
    let remove_count = directories.len().saturating_sub(retain);
    for path in directories.into_iter().take(remove_count) {
        fs::remove_dir_all(&path).map_err(|source| CompanionError::io(&path, source))?;
    }
    Ok(())
}

fn rewrite_provider_fields(
    value: &mut Value,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (key, child) in map.iter_mut() {
                if is_provider_field_key(key) {
                    if let Some(current) = child.as_str() {
                        if source_ids.contains(current) {
                            *child = Value::String(target_provider_id.to_string());
                            changed = true;
                            continue;
                        }
                    }
                }
                changed |= rewrite_provider_fields(child, source_ids, target_provider_id);
            }
            changed
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed = rewrite_provider_fields(item, source_ids, target_provider_id) || changed;
            }
            changed
        }
        _ => false,
    }
}

fn is_provider_field_key(key: &str) -> bool {
    matches!(
        key,
        "provider" | "provider_id" | "providerId" | "model_provider" | "modelProvider"
    )
}

fn create_backup_root(codex_dir: &Path) -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%9f").to_string();
    let root = codex_dir
        .join("backups")
        .join("codex-companion")
        .join(timestamp);
    fs::create_dir_all(&root).map_err(|source| CompanionError::io(&root, source))?;
    Ok(root)
}

fn backup_file(path: &Path, backup_root: &Path, codex_dir: &Path) -> Result<()> {
    backup_file_to_root(path, backup_root, codex_dir)?;
    Ok(())
}

fn backup_file_to_root(
    path: &Path,
    backup_root: &Path,
    codex_dir: &Path,
) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let relative = path.strip_prefix(codex_dir).unwrap_or(path);
    let target = backup_root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }
    fs::copy(path, &target).map_err(|source| CompanionError::io(path, source))?;
    Ok(Some(path_relative_to_codex_dir(&target, codex_dir)))
}

fn path_relative_to_codex_dir(path: &Path, codex_dir: &Path) -> String {
    path.strip_prefix(codex_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn resolve_codex_relative(codex_dir: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        codex_dir.join(path)
    }
}

fn write_config_with_auth_rollback(
    config_path: &Path,
    doc: &DocumentMut,
    auth_rollback: &AuthRollback,
    codex_dir: &Path,
) -> Result<()> {
    if let Err(source) = fs::write(config_path, doc.to_string()) {
        let write_error = CompanionError::io(config_path, source);
        if let Err(rollback_error) = auth_rollback.restore(codex_dir) {
            return Err(CompanionError::InvalidConfig(format!(
                "写入 Codex config.toml 失败: {write_error}；尝试恢复 auth.json 也失败: {rollback_error}"
            )));
        }
        return Err(write_error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_key_provider(
        auth_ref: Option<String>,
        direct_auth_ref: Option<String>,
    ) -> ProviderConfig {
        ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            websocket_url: None,
            auth_ref,
            direct_auth_ref,
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn install_writes_companion_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let relay = RelayConfig::default();
        let status =
            install_companion_provider(Some(temp.path().to_path_buf()), &relay).expect("install");
        assert!(status.installed);
        let text = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(text.contains("model_provider = \"codex-companion\""));
        assert!(text.contains("name = \"本地代理\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:17687/v1\""));
    }

    #[test]
    fn relay_install_backs_up_and_uninstall_restores_previous_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n\n[model_providers.openai]\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\n",
        )
        .expect("config");
        let relay = RelayConfig::default();

        install_companion_provider(Some(temp.path().to_path_buf()), &relay).expect("install");

        let installed = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(installed.contains("codex_companion = {"));
        assert!(installed.contains("previous_model_provider = \"openai\""));
        assert!(installed.contains("config_backup = \"backups/codex-companion/"));

        let status = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect("uninstall restores");

        assert!(!status.installed);
        assert_eq!(status.model_provider.as_deref(), Some("openai"));
        let restored = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(restored.contains("model_provider = \"openai\""));
        assert!(!restored.contains("codex_companion ="));
        assert!(!restored.contains("codex-companion"));
    }

    #[test]
    fn uninstall_allows_legacy_companion_config_without_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"codex-companion\"\n\n[model_providers.openai]\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\n\n[model_providers.codex-companion]\nname = \"本地代理\"\nbase_url = \"http://127.0.0.1:17687/v1\"\nwire_api = \"responses\"\n",
        )
        .expect("config");

        let status = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect("legacy uninstall should clean companion provider");

        assert!(!status.installed);
        assert_eq!(status.model_provider.as_deref(), Some("openai"));
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("model_provider = \"openai\""));
        assert!(!config.contains("[model_providers.codex-companion]"));
        assert!(!config.contains("codex_companion ="));
    }

    #[test]
    fn uninstall_without_marker_still_blocks_when_live_provider_is_not_companion() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n\n[model_providers.openai]\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\n\n[model_providers.codex-companion]\nname = \"本地代理\"\nbase_url = \"http://127.0.0.1:17687/v1\"\nwire_api = \"responses\"\n",
        )
        .expect("config");

        let error = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect_err("non-live legacy companion table should not be removed");

        assert!(error
            .to_string()
            .contains("没有 Companion ownership marker"));
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("[model_providers.codex-companion]"));
        assert!(config.contains("model_provider = \"openai\""));
    }

    #[test]
    fn uninstall_preserves_user_config_changed_after_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("config");
        let relay = RelayConfig::default();
        install_companion_provider(Some(temp.path().to_path_buf()), &relay).expect("install");
        let mut config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        config.push_str("\nmodel = \"manual-change\"\n");
        fs::write(temp.path().join("config.toml"), config).expect("config");

        let status = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect("manual drift should be merged during uninstall");

        assert!(!status.installed);
        let current = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(current.contains("model = \"manual-change\""));
        assert!(current.contains("model_provider = \"openai\""));
        assert!(!current.contains("codex_companion ="));
        assert!(!current.contains("codex-companion"), "{current}");
    }

    #[test]
    fn reinstall_preserves_user_config_changed_after_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("config");
        let relay = RelayConfig::default();
        install_companion_provider(Some(temp.path().to_path_buf()), &relay).expect("install");
        let mut config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        config.push_str("\nmodel = \"user-selected-model\"\n");
        fs::write(temp.path().join("config.toml"), config).expect("config");

        install_companion_provider(Some(temp.path().to_path_buf()), &relay)
            .expect("manual drift should be merged during reinstall");

        let current = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(current.contains("model = \"user-selected-model\""));
        assert!(current.contains("model_provider = \"codex-companion\""));
        assert!(current.contains("codex_companion ="));
    }

    #[test]
    fn uninstall_preserves_user_selected_provider_after_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("config");
        install_companion_provider(Some(temp.path().to_path_buf()), &RelayConfig::default())
            .expect("install");
        let mut config = fs::read_to_string(temp.path().join("config.toml"))
            .expect("managed config")
            .parse::<DocumentMut>()
            .expect("managed config TOML");
        config["model_provider"] = value("user-provider");
        config["model_providers"]["user-provider"]["name"] = value("User Provider");
        config["model_providers"]["user-provider"]["base_url"] = value("https://example.test/v1");
        fs::write(temp.path().join("config.toml"), config.to_string()).expect("config");

        uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect("user-selected provider should be preserved");

        let current = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(current.contains("model_provider = \"user-provider\""));
        assert!(current.contains("User Provider"));
        assert!(!current.contains("codex-companion"), "{current}");
        assert!(!current.contains("codex_companion ="));
    }

    #[test]
    fn relay_install_preserves_existing_auth_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "refresh_token": "refresh-token",
                "chatgpt_account_id": "account-id"
            }
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");

        let status =
            install_companion_provider(Some(temp.path().to_path_buf()), &RelayConfig::default())
                .expect("install");

        assert!(status.message.contains("Token source"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("auth");
        assert_eq!(auth, original_auth);
    }

    #[test]
    fn relay_install_records_selected_token_source_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let status = install_companion_provider_with_token_source(
            Some(temp.path().to_path_buf()),
            &RelayConfig::default(),
            Some("Companion relay injection from provider OpenRouter"),
        )
        .expect("install");

        assert!(status
            .message
            .contains("Companion relay injection from provider OpenRouter"));
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config
            .contains("token_source = \"Companion relay injection from provider OpenRouter\""));
    }

    #[test]
    fn direct_api_key_preserves_oauth_tokens_and_warns() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "refresh_token": "refresh-token",
                    "chatgpt_account_id": "account-id"
                }
            })
            .to_string(),
        )
        .expect("auth");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.message.contains("warning: direct API key mode"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("auth");
        assert!(auth.contains("\"OPENAI_API_KEY\": \"sk-test\""));
        assert!(auth.contains("\"refresh_token\": \"refresh-token\""));
        assert!(auth.contains("\"chatgpt_account_id\": \"account-id\""));
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("auth_backup = \"backups/codex-companion/"));
        assert!(config.contains("auth_write_hash = "));
    }

    #[test]
    fn preserve_official_auth_moves_third_party_api_key_into_provider_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "refresh-token"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);

        let status = install_direct_provider_with_options(
            Some(temp.path().to_path_buf()),
            &provider,
            DirectInstallOptions {
                preserve_official_codex_auth: true,
            },
        )
        .expect("preserve setting should use config-only auth");

        assert!(status
            .message
            .contains("provider-scoped experimental_bearer_token"));
        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            original_auth
        );
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("model_provider = \"openrouter\""));
        assert!(config.contains("experimental_bearer_token = \"sk-test\""));
        assert!(!config.contains("auth_write_hash = \""));
    }

    #[test]
    fn relay_install_restores_previous_direct_auth_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "refresh_token": "refresh-token",
                "chatgpt_account_id": "account-id"
            }
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);

        install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");
        assert!(fs::read_to_string(temp.path().join("auth.json"))
            .expect("auth")
            .contains("\"OPENAI_API_KEY\": \"sk-test\""));

        let status =
            install_companion_provider(Some(temp.path().to_path_buf()), &RelayConfig::default())
                .expect("install relay");

        assert!(status.message.contains("auth.json 未被本地代理写入"));
        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            original_auth
        );
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(!config.contains("auth_write_hash ="));
    }

    #[test]
    fn direct_after_relay_restore_backs_up_current_auth_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "old-refresh"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        let first_auth_path = temp.path().join("first-provider-auth.json");
        fs::write(&first_auth_path, r#"{"api_key":"sk-first"}"#).expect("provider auth");
        let first_provider =
            api_key_provider(Some(format!("file:{}", first_auth_path.display())), None);

        install_direct_provider(Some(temp.path().to_path_buf()), &first_provider)
            .expect("install direct");
        install_companion_provider(Some(temp.path().to_path_buf()), &RelayConfig::default())
            .expect("install relay");

        let current_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "new-refresh"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &current_auth).expect("auth");
        let second_auth_path = temp.path().join("second-provider-auth.json");
        fs::write(&second_auth_path, r#"{"api_key":"sk-second"}"#).expect("provider auth");
        let second_provider =
            api_key_provider(Some(format!("file:{}", second_auth_path.display())), None);

        install_direct_provider(Some(temp.path().to_path_buf()), &second_provider)
            .expect("install direct again");
        uninstall_companion_provider(Some(temp.path().to_path_buf())).expect("uninstall");

        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            current_auth
        );
    }

    #[test]
    fn uninstall_validates_config_backup_before_restoring_auth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "refresh-token"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("config");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);
        install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        let doc = config.parse::<DocumentMut>().expect("config doc");
        let config_backup = CompanionConfigMarker::from_doc(&doc)
            .and_then(|marker| marker.config_backup)
            .expect("config backup");
        fs::remove_file(temp.path().join(config_backup)).expect("remove backup");

        let error = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect_err("missing config backup should block before auth restore");

        assert!(error.to_string().contains("config backup 不存在"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("auth");
        assert!(auth.contains("\"OPENAI_API_KEY\": \"sk-test\""));
    }

    #[test]
    fn uninstall_rolls_back_config_when_auth_restore_fails_after_config_restore() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "refresh-token"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openai\"\n",
        )
        .expect("config");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);
        install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");
        let managed_config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        let managed_auth = fs::read_to_string(temp.path().join("auth.json")).expect("auth");
        let auth_backup = CompanionConfigMarker::from_doc(
            &managed_config
                .parse::<DocumentMut>()
                .expect("managed config doc"),
        )
        .and_then(|marker| marker.auth_backup)
        .expect("auth backup");
        let auth_backup_path = temp.path().join(auth_backup);
        fs::remove_file(&auth_backup_path).expect("remove auth backup file");
        fs::create_dir_all(&auth_backup_path).expect("replace auth backup with directory");

        let error = uninstall_companion_provider(Some(temp.path().to_path_buf()))
            .expect_err("auth restore failure should roll back restored config");

        assert!(!error.to_string().is_empty());
        assert_eq!(
            fs::read_to_string(temp.path().join("config.toml")).expect("config"),
            managed_config
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            managed_auth
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_install_rolls_back_auth_when_config_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("config.toml");
        fs::write(&config_path, "model_provider = \"openai\"\n").expect("config");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o444))
            .expect("readonly config");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "refresh-token"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);

        install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect_err("readonly config should fail");

        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            original_auth
        );
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o644))
            .expect("restore config permissions");
    }

    #[cfg(unix)]
    #[test]
    fn relay_install_rolls_back_auth_restore_when_config_write_fails() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let original_auth = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {"refresh_token": "refresh-token"}
        })
        .to_string();
        fs::write(temp.path().join("auth.json"), &original_auth).expect("auth");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);
        install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");
        let managed_auth = fs::read_to_string(temp.path().join("auth.json")).expect("auth");
        assert!(managed_auth.contains("\"OPENAI_API_KEY\": \"sk-test\""));
        let config_path = temp.path().join("config.toml");
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o444))
            .expect("readonly config");

        install_companion_provider(Some(temp.path().to_path_buf()), &RelayConfig::default())
            .expect_err("readonly config should fail");

        assert_eq!(
            fs::read_to_string(temp.path().join("auth.json")).expect("auth"),
            managed_auth
        );
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o644))
            .expect("restore config permissions");
    }

    #[test]
    fn direct_env_install_restores_previous_direct_auth_write() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
        let file_provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);
        install_direct_provider(Some(temp.path().to_path_buf()), &file_provider)
            .expect("install direct file");
        assert!(temp.path().join("auth.json").exists());

        let env_provider = api_key_provider(
            Some("env:OPENROUTER_API_KEY".to_string()),
            Some("env:OPENROUTER_API_KEY".to_string()),
        );
        let status = install_direct_provider(Some(temp.path().to_path_buf()), &env_provider)
            .expect("install direct env");

        assert!(status
            .message
            .contains("prior Companion auth.json write was restored"));
        assert!(!temp.path().join("auth.json").exists());
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("env_key = \"OPENROUTER_API_KEY\""));
        assert!(!config.contains("auth_write_hash ="));
    }

    #[test]
    fn direct_api_key_warns_for_supported_oauth_auth_shapes() {
        for auth in [
            serde_json::json!({"refresh_token": "top-level-refresh"}),
            serde_json::json!({"credentials": {"refresh_token": "credential-refresh"}}),
            serde_json::json!({
                "accounts": [{
                    "platform": "openai",
                    "type": "oauth",
                    "tokens": {"refresh_token": "account-refresh"}
                }]
            }),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            fs::write(temp.path().join("auth.json"), auth.to_string()).expect("auth");
            let auth_path = temp.path().join("provider-auth.json");
            fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("provider auth");
            let provider = api_key_provider(Some(format!("file:{}", auth_path.display())), None);

            let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
                .expect("install direct");

            assert!(
                status.message.contains("warning: direct API key mode"),
                "missing warning for auth shape: {}",
                auth
            );
        }
    }

    #[test]
    fn dry_run_does_not_write_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: true,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.source_provider_ids, vec!["openai".to_string()]);
        assert_eq!(outcome.plan.history_files, 1);
        assert_eq!(outcome.plan.history_lines, 1);
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openai\""));
        assert!(!repair_backup_parent(temp.path()).exists());
    }

    #[test]
    fn repair_transaction_rolls_back_files_after_later_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        let original = "{\"type\":\"session_meta\",\"payload\":{\"model_provider\":\"openai\"}}\n";
        fs::write(&file, original).expect("write original");

        let error = run_repair_transaction(temp.path(), |backup_root| -> Result<()> {
            let root = ensure_repair_backup_root(backup_root, temp.path())?;
            backup_file(&file, root, temp.path())?;
            fs::write(
                &file,
                "{\"type\":\"session_meta\",\"payload\":{\"model_provider\":\"codex-companion\"}}\n",
            )
            .map_err(|source| CompanionError::io(&file, source))?;
            Err(CompanionError::InvalidConfig(
                "simulated later repair failure".to_string(),
            ))
        })
        .expect_err("repair should fail");

        assert!(error.to_string().contains("已从"));
        assert_eq!(fs::read_to_string(&file).expect("restored file"), original);
        let backups = fs::read_dir(repair_backup_parent(temp.path()))
            .expect("repair backups")
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn repair_backup_cleanup_keeps_newest_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = repair_backup_parent(temp.path());
        fs::create_dir_all(&parent).expect("repair parent");
        for name in ["001", "002", "003", "004", "005"] {
            fs::create_dir(parent.join(name)).expect("backup dir");
        }

        cleanup_old_repair_backups(temp.path(), 3).expect("cleanup");

        let mut names = fs::read_dir(&parent)
            .expect("repair backups")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["003", "004", "005"]);
    }

    #[test]
    fn repair_ignores_its_own_backups() {
        let temp = tempfile::tempdir().expect("tempdir");
        let backup_sessions = temp
            .path()
            .join("backups")
            .join("codex-companion")
            .join("old")
            .join("sessions");
        fs::create_dir_all(&backup_sessions).expect("backup sessions");
        let backup_file = backup_sessions.join("session.jsonl");
        fs::write(
            &backup_file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert!(outcome.backup_root.is_none());
        assert!(outcome.skipped_reason.is_some());
        assert_eq!(outcome.plan.history_files, 0);
        assert_eq!(outcome.plan.history_lines, 0);
        let text = fs::read_to_string(&backup_file).expect("read");
        assert!(text.contains("\"openai\""));
    }

    #[test]
    fn repair_rewrites_history_and_merges_sqlite_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\",\"cwd\":\"/work/project\"}}\n",
        )
        .expect("write");

        let db = temp.path().join(CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, cwd TEXT NOT NULL, rollout_path TEXT NOT NULL);
             INSERT INTO threads (id, model_provider, cwd, rollout_path) VALUES ('s1', 'openai', '', '');",
        )
        .expect("schema");
        drop(conn);

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.history_lines, 1);
        assert_eq!(outcome.plan.state_rows, 1);
        assert_eq!(outcome.migrated_history_lines, 1);
        assert_eq!(outcome.migrated_state_rows, 1);
        assert!(outcome.backup_root.is_some());
        assert!(outcome.skipped_reason.is_none());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"codex-companion\""));
        let conn = Connection::open(&db).expect("db");
        let (provider, cwd): (String, String) = conn
            .query_row(
                "SELECT model_provider, cwd FROM threads WHERE id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("provider");
        assert_eq!(provider, COMPANION_PROVIDER_ID);
        assert_eq!(cwd, "/work/project");
    }

    #[test]
    fn repair_merges_missing_project_for_target_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"codex-companion\",\"cwd\":\"/work/project\"}}\n",
        )
        .expect("write");

        let db = temp.path().join(CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, cwd TEXT NOT NULL, rollout_path TEXT NOT NULL);
             INSERT INTO threads (id, model_provider, cwd, rollout_path) VALUES ('s1', 'codex-companion', '', '');",
        )
        .expect("schema");
        drop(conn);

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert!(outcome.backup_root.is_some());
        assert_eq!(outcome.plan.state_rows, 1);
        assert_eq!(outcome.migrated_history_lines, 0);
        assert_eq!(outcome.migrated_state_rows, 1);
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"codex-companion\""));
        let conn = Connection::open(&db).expect("db");
        let cwd: String = conn
            .query_row("SELECT cwd FROM threads WHERE id = 's1'", [], |row| {
                row.get(0)
            })
            .expect("cwd");
        assert_eq!(cwd, "/work/project");
    }

    #[test]
    fn repair_rewrites_history_for_custom_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some("openrouter".to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.target_provider_id, "openrouter");
        assert_eq!(outcome.migrated_history_lines, 1);
        assert!(outcome.backup_root.is_some());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openrouter\""));
    }

    #[test]
    fn repair_defaults_to_current_codex_model_provider_and_rewrites_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openrouter\"\n",
        )
        .expect("config");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: None,
        })
        .expect("repair");

        assert_eq!(outcome.plan.target_provider_id, "openrouter");
        assert_eq!(outcome.migrated_history_lines, 1);
        assert!(outcome.backup_root.is_some());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openrouter\""));
    }

    #[test]
    fn repair_defaults_official_auth_without_model_provider_to_openai() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("config.toml"), "").expect("config");
        fs::write(
            temp.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":null,"tokens":{"access_token":"access-token"}}"#,
        )
        .expect("auth");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        fs::write(
            sessions.join("session.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"cc-switch-bucket\",\"cwd\":\"/work/project\"}}\n",
        )
        .expect("write");
        let db = temp.path().join(CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, cwd TEXT NOT NULL, rollout_path TEXT NOT NULL);
             INSERT INTO threads (id, model_provider, cwd, rollout_path) VALUES ('s1', 'cc-switch-bucket', '', '');",
        )
        .expect("schema");
        drop(conn);

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: None,
        })
        .expect("repair");

        assert_eq!(outcome.plan.target_provider_id, "openai");
        assert_eq!(
            outcome.plan.source_provider_ids,
            vec!["cc-switch-bucket".to_string()]
        );
        assert_eq!(outcome.migrated_state_rows, 1);
        assert_eq!(outcome.migrated_history_lines, 1);
        let text = fs::read_to_string(sessions.join("session.jsonl")).expect("session");
        assert!(text.contains("\"openai\""));
        let conn = Connection::open(&db).expect("db");
        let provider: String = conn
            .query_row(
                "SELECT model_provider FROM threads WHERE id = 's1'",
                [],
                |row| row.get(0),
            )
            .expect("provider");
        assert_eq!(provider, "openai");
    }

    #[test]
    fn repair_history_scan_ignores_non_session_jsonl_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        let random = temp.path().join("logs");
        fs::create_dir_all(&sessions).expect("sessions");
        fs::create_dir_all(&random).expect("logs");
        fs::write(
            sessions.join("session.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"old-bucket\"}}\n",
        )
        .expect("session");
        fs::write(
            random.join("not-a-session.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s2\",\"model_provider\":\"backup-bucket\"}}\n",
        )
        .expect("random");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: true,
            target_provider_id: Some("openai".to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.history_files, 1);
        assert_eq!(
            outcome.plan.source_provider_ids,
            vec!["old-bucket".to_string()]
        );
    }

    #[test]
    fn plugin_only_repair_collects_provider_ids_from_plugins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("plugin-state");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let file = plugin_dir.join("plugin.json");
        fs::write(
            &file,
            r#"{"provider":"openai","nested":{"modelProvider":"custom"}}"#,
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: false,
            plugins: true,
            dry_run: false,
            target_provider_id: Some("codex-companion".to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.migrated_plugin_files, 1);
        assert_eq!(
            outcome.plan.source_provider_ids,
            vec!["custom".to_string(), "openai".to_string()]
        );
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains(r#""provider": "codex-companion""#));
        assert!(text.contains(r#""modelProvider": "codex-companion""#));
    }

    #[test]
    fn install_direct_provider_writes_native_codex_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let provider = ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            websocket_url: None,
            auth_ref: Some("env:OPENROUTER_API_KEY".to_string()),
            direct_auth_ref: Some("env:OPENROUTER_API_KEY".to_string()),
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.installed);
        assert_eq!(status.model_provider.as_deref(), Some("openrouter"));
        let text = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(text.contains("model_provider = \"openrouter\""));
        assert!(text.contains("base_url = \"https://openrouter.ai/api/v1\""));
        assert!(text.contains("env_key = \"OPENROUTER_API_KEY\""));
        assert!(text.contains("requires_openai_auth = true"));
        assert!(!text.contains("api_key_env_var"));
    }

    #[test]
    fn install_direct_provider_with_file_key_writes_codex_auth_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("auth");
        let provider = ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.installed);
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("model_provider = \"openrouter\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(!config.contains("env_key ="));
        assert!(!config.contains("api_key_env_var"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("codex auth");
        assert!(auth.contains("\"OPENAI_API_KEY\": \"sk-test\""));
    }

    #[test]
    fn install_direct_official_provider_writes_codex_oauth_tokens() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            r#"
model_provider = "old-direct"

[model_providers.old-direct]
name = "Old Direct"
base_url = "https://old.example/v1"

[model_providers.keep-me]
name = "Keep Me"
base_url = "https://keep.example/v1"
"#,
        )
        .expect("config");
        let auth_path = temp.path().join("official-auth.json");
        fs::write(
            &auth_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "access-token",
                    "id_token": "id-token",
                    "refresh_token": "refresh-token",
                    "chatgpt_account_id": "account-id",
                    "email": "mark@example.com",
                    "plan_type": "team"
                }
            })
            .to_string(),
        )
        .expect("auth");
        let provider = ProviderConfig {
            id: "official-mark".to_string(),
            name: "mark@example.com".to_string(),
            kind: codex_companion_core::ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 50,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.installed);
        assert_eq!(status.model_provider.as_deref(), Some("openai"));
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("model_provider = \"openai\""));
        assert!(config.contains("[model_providers.old-direct]"));
        assert!(!config.contains("[model_providers.official-mark]"));
        assert!(config.contains("[model_providers.keep-me]"));
        assert!(!config.contains("openai_base_url"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("codex auth");
        assert!(auth.contains("\"OPENAI_API_KEY\": null"));
        assert!(auth.contains("\"access_token\": \"access-token\""));
        assert!(auth.contains("\"refresh_token\": \"refresh-token\""));
        assert!(auth.contains("\"chatgpt_account_id\": \"account-id\""));
    }
}
