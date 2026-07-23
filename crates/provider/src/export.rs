use crate::types::{ProviderExportFormat, ProviderExportOutput};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{SecondsFormat, TimeZone, Utc};
use codex_companion_core::{
    CompanionError, ConfigStore, ProviderAccountInfo, ProviderConfig, ProviderKind, Result,
};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;

pub fn export_provider_json(
    store: &ConfigStore,
    id: &str,
    format: Option<ProviderExportFormat>,
) -> Result<ProviderExportOutput> {
    let config = store.load()?;
    let provider = config
        .providers
        .get(id)
        .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown provider: {id}")))?;
    let format = format.unwrap_or(ProviderExportFormat::CodexCompanion);
    let json_value = if matches!(provider.kind, ProviderKind::OfficialCodex) {
        export_official_provider(provider, format)?
    } else {
        export_api_key_provider(provider)?
    };
    let json_content = serde_json::to_string_pretty(&json_value).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider export serialize failed: {source}"))
    })?;
    Ok(ProviderExportOutput {
        file_name_base: export_file_name_base(provider, format),
        json_content,
    })
}

fn export_api_key_provider(provider: &ProviderConfig) -> Result<Value> {
    let api_key = resolve_api_key(provider)?;
    let account = provider.account.as_ref();
    let mut item = Map::new();
    item.insert("auth_mode".to_string(), Value::String("apikey".to_string()));
    item.insert("OPENAI_API_KEY".to_string(), Value::String(api_key));
    item.insert(
        "email".to_string(),
        Value::String(account_email_or_name(account, provider)),
    );
    item.insert(
        "api_base_url".to_string(),
        Value::String(provider.base_url.clone()),
    );
    item.insert(
        "api_provider_id".to_string(),
        Value::String(provider.id.clone()),
    );
    item.insert(
        "api_provider_name".to_string(),
        Value::String(provider.name.clone()),
    );
    insert_optional(&mut item, "websocket_url", provider.websocket_url.clone());
    Ok(Value::Array(vec![Value::Object(item)]))
}

fn export_official_provider(
    provider: &ProviderConfig,
    format: ProviderExportFormat,
) -> Result<Value> {
    let auth = read_auth_value(provider.auth_ref.as_deref())?;
    if is_agent_identity_auth(&auth) {
        return export_agent_identity_provider(provider, &auth, format);
    }
    let access_token = pick_json_string(
        &auth,
        &[
            &["tokens", "access_token"],
            &["credentials", "access_token"],
            &["access_token"],
            &["token"],
        ],
    )
    .ok_or_else(|| {
        CompanionError::InvalidConfig("官方账号缺少 access_token，无法导出".to_string())
    })?;
    let id_token = pick_json_string(
        &auth,
        &[
            &["tokens", "id_token"],
            &["credentials", "id_token"],
            &["id_token"],
        ],
    );
    let refresh_token = pick_json_string(
        &auth,
        &[
            &["tokens", "refresh_token"],
            &["credentials", "refresh_token"],
            &["refresh_token"],
        ],
    );
    let account = provider.account.as_ref();
    let account_id = account
        .and_then(|info| info.account_id.clone())
        .or_else(|| {
            pick_json_string(
                &auth,
                &[
                    &["tokens", "chatgpt_account_id"],
                    &["tokens", "account_id"],
                    &["credentials", "chatgpt_account_id"],
                    &["credentials", "account_id"],
                    &["chatgpt_account_id"],
                    &["account_id"],
                ],
            )
        })
        .unwrap_or_default();
    let email = account_email_or_name(account, provider);
    let expired = pick_json_string(
        &auth,
        &[
            &["tokens", "expired"],
            &["expired"],
            &["credentials", "expires_at"],
            &["expires_at"],
        ],
    )
    .or_else(|| jwt_expiry_iso(&access_token))
    .unwrap_or_default();
    let last_refresh = pick_json_string(
        &auth,
        &[
            &["tokens", "last_refresh"],
            &["last_refresh"],
            &["lastRefresh"],
        ],
    )
    .or_else(|| account.and_then(|info| info.last_refresh_at.clone()))
    .unwrap_or_else(now_iso);

    match format {
        ProviderExportFormat::Sub2api => {
            let mut credentials = Map::new();
            credentials.insert("access_token".to_string(), Value::String(access_token));
            if !expired.is_empty() {
                credentials.insert("expires_at".to_string(), Value::String(expired));
            }
            insert_optional(&mut credentials, "refresh_token", refresh_token);
            insert_optional(&mut credentials, "id_token", id_token);
            if !email.is_empty() {
                credentials.insert("email".to_string(), Value::String(email));
            }
            if !account_id.is_empty() {
                credentials.insert("chatgpt_account_id".to_string(), Value::String(account_id));
            }
            insert_optional(
                &mut credentials,
                "chatgpt_user_id",
                account.and_then(|info| info.user_id.clone()).or_else(|| {
                    pick_json_string(
                        &auth,
                        &[
                            &["tokens", "chatgpt_user_id"],
                            &["tokens", "user_id"],
                            &["credentials", "chatgpt_user_id"],
                            &["credentials", "user_id"],
                            &["chatgpt_user_id"],
                            &["user_id"],
                        ],
                    )
                }),
            );
            insert_optional(
                &mut credentials,
                "plan_type",
                account
                    .and_then(|info| info.subscription_type.clone())
                    .or_else(|| {
                        pick_json_string(
                            &auth,
                            &[
                                &["tokens", "plan_type"],
                                &["credentials", "plan_type"],
                                &["plan_type"],
                            ],
                        )
                    }),
            );
            insert_optional(
                &mut credentials,
                "subscription_expires_at",
                account.and_then(|info| info.valid_until.clone()),
            );
            Ok(json!({
                "exported_at": now_iso(),
                "proxies": [],
                "accounts": [{
                    "name": provider.account.as_ref().and_then(|info| info.display_name.clone()).unwrap_or_else(|| provider.name.clone()),
                    "platform": "openai",
                    "type": "oauth",
                    "credentials": Value::Object(credentials),
                    "concurrency": 0,
                    "priority": 0
                }],
                "type": "sub2api-data",
                "version": 1
            }))
        }
        ProviderExportFormat::CodexCompanion => Ok(Value::Array(vec![portable_token_storage(
            provider,
            access_token,
            id_token,
            refresh_token,
            account_id,
            last_refresh,
            email,
            expired,
        )])),
        ProviderExportFormat::Cpa => Ok(portable_token_storage(
            provider,
            access_token,
            id_token,
            refresh_token,
            account_id,
            last_refresh,
            email,
            expired,
        )),
    }
}

fn is_agent_identity_auth(auth: &Value) -> bool {
    pick_json_string(
        auth,
        &[&["auth_mode"], &["authMode"], &["openai_auth_mode"]],
    )
    .is_some_and(|mode| mode.eq_ignore_ascii_case("agentIdentity"))
}

fn export_agent_identity_provider(
    provider: &ProviderConfig,
    auth: &Value,
    format: ProviderExportFormat,
) -> Result<Value> {
    if matches!(format, ProviderExportFormat::Cpa) {
        return Err(CompanionError::InvalidConfig(
            "CPA 格式不支持 Agent Identity，请使用 Codex Companion 或 Sub2API".to_string(),
        ));
    }
    let runtime_id = pick_json_string(auth, &[&["agent_runtime_id"], &["agentRuntimeId"]])
        .ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity 缺少 agent_runtime_id".to_string())
        })?;
    let private_key = pick_json_string(auth, &[&["agent_private_key"], &["agentPrivateKey"]])
        .ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity 缺少 agent_private_key".to_string())
        })?;
    let task_id = pick_json_string(auth, &[&["task_id"], &["taskId"]]);
    let account = provider.account.as_ref();
    let account_id = account
        .and_then(|info| info.account_id.clone())
        .or_else(|| pick_json_string(auth, &[&["chatgpt_account_id"], &["account_id"]]))
        .unwrap_or_default();
    let user_id = account
        .and_then(|info| info.user_id.clone())
        .or_else(|| pick_json_string(auth, &[&["chatgpt_user_id"], &["user_id"]]));
    let email = account_email_or_name(account, provider);
    let plan_type = account
        .and_then(|info| info.subscription_type.clone())
        .or_else(|| pick_json_string(auth, &[&["plan_type"], &["chatgpt_plan_type"]]));

    let mut credentials = Map::new();
    credentials.insert(
        "auth_mode".to_string(),
        Value::String("agentIdentity".to_string()),
    );
    credentials.insert("agent_runtime_id".to_string(), Value::String(runtime_id));
    credentials.insert("agent_private_key".to_string(), Value::String(private_key));
    insert_optional(&mut credentials, "task_id", task_id);
    if !account_id.is_empty() {
        credentials.insert("chatgpt_account_id".to_string(), Value::String(account_id));
    }
    insert_optional(&mut credentials, "chatgpt_user_id", user_id);
    if !email.is_empty() {
        credentials.insert("email".to_string(), Value::String(email));
    }
    insert_optional(&mut credentials, "plan_type", plan_type);

    match format {
        ProviderExportFormat::CodexCompanion => Ok(Value::Array(vec![json!({
            "auth_mode": "agentIdentity",
            "agent_identity": Value::Object(credentials),
        })])),
        ProviderExportFormat::Sub2api => Ok(json!({
            "exported_at": now_iso(),
            "proxies": [],
            "accounts": [{
                "name": provider.name,
                "platform": "openai",
                "type": "agent_identity",
                "credentials": Value::Object(credentials),
                "concurrency": 0,
                "priority": 0
            }],
            "type": "sub2api-data",
            "version": 1
        })),
        ProviderExportFormat::Cpa => unreachable!(),
    }
}

#[allow(clippy::too_many_arguments)]
fn portable_token_storage(
    _provider: &ProviderConfig,
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
    account_id: String,
    last_refresh: String,
    email: String,
    expired: String,
) -> Value {
    json!({
        "id_token": id_token.unwrap_or_default(),
        "access_token": access_token,
        "refresh_token": refresh_token.unwrap_or_default(),
        "account_id": account_id,
        "last_refresh": last_refresh,
        "email": email,
        "type": "codex",
        "expired": expired
    })
}

fn resolve_api_key(provider: &ProviderConfig) -> Result<String> {
    let auth_ref = provider
        .auth_ref
        .as_deref()
        .or(provider.direct_auth_ref.as_deref())
        .ok_or_else(|| {
            CompanionError::InvalidConfig("API Key provider 缺少 auth_ref".to_string())
        })?;
    if let Some(name) = auth_ref.strip_prefix("env:") {
        return std::env::var(name).map_err(|_| {
            CompanionError::InvalidConfig(format!(
                "该 provider 使用环境变量 {name}，当前进程未读取到密钥值，无法导出固定 API Key JSON"
            ))
        });
    }
    let value = read_auth_value(Some(auth_ref))?;
    pick_json_string(
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
        CompanionError::InvalidConfig("API Key auth 文件缺少 OPENAI_API_KEY/api_key".to_string())
    })
}

fn read_auth_value(auth_ref: Option<&str>) -> Result<Value> {
    let auth_ref = auth_ref
        .and_then(|value| value.strip_prefix("file:"))
        .ok_or_else(|| {
            CompanionError::InvalidConfig("provider 缺少可导出的文件 auth_ref".to_string())
        })?;
    let path = PathBuf::from(auth_ref);
    let text = fs::read_to_string(&path).map_err(|source| CompanionError::io(&path, source))?;
    serde_json::from_str::<Value>(&text).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "解析 provider auth 文件失败 {}: {source}",
            path.display()
        ))
    })
}

fn account_email_or_name(
    account: Option<&ProviderAccountInfo>,
    provider: &ProviderConfig,
) -> String {
    account
        .and_then(|info| info.email.clone())
        .or_else(|| account.and_then(|info| info.display_name.clone()))
        .unwrap_or_else(|| provider.name.clone())
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        map.insert(key.to_string(), Value::String(value));
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

fn jwt_expiry_iso(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<Value>(&bytes).ok()?;
    let exp = value.get("exp")?.as_i64()?;
    let date = Utc.timestamp_opt(exp, 0).single()?;
    Some(date.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn export_file_name_base(provider: &ProviderConfig, format: ProviderExportFormat) -> String {
    let base = sanitize_file_name(&provider.name, &provider.id);
    match format {
        ProviderExportFormat::CodexCompanion => base,
        ProviderExportFormat::Sub2api => format!("{base}_sub2api"),
        ProviderExportFormat::Cpa => format!("{base}_cpa"),
    }
}

fn sanitize_file_name(value: &str, fallback: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::add_provider;
    use crate::types::ProviderUpsert;
    use codex_companion_core::{default_refresh_interval_seconds, ProviderAccountInfo};
    use std::collections::BTreeMap;

    fn provider(
        id: &str,
        kind: ProviderKind,
        auth_ref: String,
        account: Option<ProviderAccountInfo>,
    ) -> ProviderUpsert {
        ProviderUpsert {
            id: id.to_string(),
            name: id.to_string(),
            kind,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: Some("wss://api.example.com/v1/responses".to_string()),
            auth_ref: Some(auth_ref),
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account,
        }
    }

    #[test]
    fn exports_api_key_provider_as_codex_companion_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let auth_path = temp.path().join("api-key.json");
        fs::write(
            &auth_path,
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-secret"}"#,
        )
        .expect("auth");
        add_provider(
            &store,
            provider(
                "api-key-provider",
                ProviderKind::RelayProvider,
                format!("file:{}", auth_path.display()),
                Some(ProviderAccountInfo {
                    email: Some("api-key@example.com".to_string()),
                    ..ProviderAccountInfo::default()
                }),
            ),
        )
        .expect("add");

        let output = export_provider_json(
            &store,
            "api-key-provider",
            Some(ProviderExportFormat::CodexCompanion),
        )
        .expect("export");
        let value = serde_json::from_str::<Value>(&output.json_content).expect("json");
        let item = value
            .as_array()
            .and_then(|items| items.first())
            .expect("item");

        assert_eq!(item["auth_mode"], "apikey");
        assert_eq!(item["OPENAI_API_KEY"], "sk-secret");
        assert_eq!(item["api_provider_id"], "api-key-provider");
        assert_eq!(item["api_base_url"], "https://api.example.com/v1");
        assert_eq!(item["websocket_url"], "wss://api.example.com/v1/responses");
    }

    #[test]
    fn exports_official_provider_as_cpa_and_sub2api() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let auth_path = temp.path().join("official.json");
        fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"access-token","id_token":"id-token","refresh_token":"refresh-token","account_id":"acc-1","last_refresh":"2026-06-08T12:00:00Z"}}"#,
        )
        .expect("auth");
        add_provider(
            &store,
            provider(
                "official-provider",
                ProviderKind::OfficialCodex,
                format!("file:{}", auth_path.display()),
                Some(ProviderAccountInfo {
                    email: Some("codex@example.com".to_string()),
                    account_id: Some("acc-1".to_string()),
                    subscription_type: Some("TEAM".to_string()),
                    ..ProviderAccountInfo::default()
                }),
            ),
        )
        .expect("add");

        let companion = export_provider_json(
            &store,
            "official-provider",
            Some(ProviderExportFormat::CodexCompanion),
        )
        .expect("companion");
        let companion_value =
            serde_json::from_str::<Value>(&companion.json_content).expect("companion json");
        assert_eq!(companion_value[0]["access_token"], "access-token");
        assert_eq!(companion_value[0]["type"], "codex");

        let cpa =
            export_provider_json(&store, "official-provider", Some(ProviderExportFormat::Cpa))
                .expect("cpa");
        let cpa_value = serde_json::from_str::<Value>(&cpa.json_content).expect("cpa json");
        assert_eq!(cpa_value["access_token"], "access-token");
        assert_eq!(cpa_value["refresh_token"], "refresh-token");
        assert_eq!(cpa_value["account_id"], "acc-1");
        assert_eq!(cpa_value["type"], "codex");

        let sub2api = export_provider_json(
            &store,
            "official-provider",
            Some(ProviderExportFormat::Sub2api),
        )
        .expect("sub2api");
        let sub2api_value = serde_json::from_str::<Value>(&sub2api.json_content).expect("sub json");
        assert_eq!(sub2api_value["type"], "sub2api-data");
        assert_eq!(
            sub2api_value["accounts"][0]["credentials"]["access_token"],
            "access-token"
        );
        assert_eq!(
            sub2api_value["accounts"][0]["credentials"]["plan_type"],
            "TEAM"
        );
    }
}
