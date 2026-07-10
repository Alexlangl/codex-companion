use crate::registry::add_provider;
use crate::types::{
    ProviderImportDraft, ProviderImportOutcome, ProviderUpsert, OFFICIAL_CODEX_BASE_URL,
};
use codex_companion_core::{
    default_codex_dir, default_refresh_interval_seconds, CompanionError, ConfigStore,
    ProviderAccountInfo, ProviderKind, ProviderQuotaWindow, Result, COMPANION_PROVIDER_ID,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

pub fn import_provider_json(
    store: &ConfigStore,
    json_text: &str,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<ProviderImportOutcome> {
    let value = serde_json::from_str::<serde_json::Value>(json_text).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON parse failed: {source}"))
    })?;
    if is_auth_mode_api_key(&value) || extract_api_key(&value).is_some() {
        return import_api_key_provider_from_json(store, &value, provider_id, provider_name);
    }
    let draft =
        parse_provider_import_draft(&value, provider_id.as_deref(), provider_name.as_deref())?;
    let auth_path = store
        .data_dir()
        .join("auth")
        .join("accounts")
        .join(format!("{}.json", draft.provider_id));
    if let Some(parent) = auth_path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }

    let auth = extract_codex_oauth_auth(&value).ok_or_else(unsupported_import_error)?;
    let account = extract_provider_account_info(&value, &auth);
    let text = serde_json::to_string_pretty(&auth).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON serialize failed: {source}"))
    })?;
    fs::write(&auth_path, format!("{text}\n"))
        .map_err(|source| CompanionError::io(&auth_path, source))?;

    let mut model_map = BTreeMap::new();
    if let Some(model) = draft.model.as_ref() {
        model_map.insert(model.clone(), model.clone());
    }
    let existed = store.load()?.providers.contains_key(&draft.provider_id);
    let provider = add_provider(
        store,
        ProviderUpsert {
            id: draft.provider_id,
            name: draft.provider_name,
            kind: ProviderKind::OfficialCodex,
            base_url: draft.base_url,
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map,
            priority: 50,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: Some(account),
        },
    )?;

    Ok(ProviderImportOutcome {
        provider,
        import_kind: draft.import_kind,
        account_id: draft.account_id,
        auth_path,
        created: !existed,
        message: if existed {
            "已更新 Codex 官方账号 provider".to_string()
        } else {
            "已导入 Codex 官方账号 provider".to_string()
        },
    })
}

pub fn import_provider_json_many(
    store: &ConfigStore,
    json_text: &str,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<Vec<ProviderImportOutcome>> {
    let value = serde_json::from_str::<serde_json::Value>(json_text).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON parse failed: {source}"))
    })?;
    if let Some(items) = value.as_array().filter(|items| !items.is_empty()) {
        if provider_id
            .as_deref()
            .and_then(normalize_non_empty)
            .is_some()
        {
            return Err(CompanionError::InvalidConfig(
                "批量 provider JSON 不能同时指定单个 provider id".to_string(),
            ));
        }
        return items
            .iter()
            .map(|item| import_provider_json(store, &item.to_string(), None, provider_name.clone()))
            .collect();
    }
    let Some(accounts) = value
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .filter(|accounts| accounts.len() > 1)
    else {
        return import_provider_json(store, json_text, provider_id, provider_name)
            .map(|outcome| vec![outcome]);
    };
    if provider_id
        .as_deref()
        .and_then(normalize_non_empty)
        .is_some()
    {
        return Err(CompanionError::InvalidConfig(
            "批量账号 JSON 不能同时指定单个 provider id".to_string(),
        ));
    }

    let mut outcomes = Vec::new();
    for account in accounts {
        let mut item = value.clone();
        if let Some(object) = item.as_object_mut() {
            object.insert(
                "accounts".to_string(),
                serde_json::Value::Array(vec![account.clone()]),
            );
        }
        outcomes.push(import_provider_json(
            store,
            &item.to_string(),
            None,
            provider_name.clone(),
        )?);
    }
    Ok(outcomes)
}

pub fn import_api_key_provider(
    store: &ConfigStore,
    provider_name: String,
    kind: ProviderKind,
    base_url: String,
    api_key: String,
    env_var: Option<String>,
    model: Option<String>,
    refresh_interval_seconds: Option<u64>,
) -> Result<ProviderImportOutcome> {
    import_api_key_provider_with_metadata(
        store,
        None,
        provider_name,
        kind,
        base_url,
        api_key,
        env_var,
        model,
        refresh_interval_seconds,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn import_api_key_provider_with_metadata(
    store: &ConfigStore,
    provider_id: Option<String>,
    provider_name: String,
    kind: ProviderKind,
    base_url: String,
    api_key: String,
    env_var: Option<String>,
    model: Option<String>,
    refresh_interval_seconds: Option<u64>,
    account_email: Option<String>,
) -> Result<ProviderImportOutcome> {
    let provider_name = normalize_non_empty(&provider_name)
        .ok_or_else(|| CompanionError::InvalidConfig("provider 名称不能为空".to_string()))?;
    let api_key = normalize_non_empty(&api_key);
    let env_var = env_var.as_deref().and_then(normalize_non_empty);
    if api_key.is_none() && env_var.is_none() {
        return Err(CompanionError::InvalidConfig(
            "API Key 和环境变量名至少需要填写一个".to_string(),
        ));
    }
    let model = model.as_deref().and_then(normalize_non_empty);
    let provider_id = provider_id
        .as_deref()
        .and_then(sanitize_provider_id)
        .unwrap_or_else(|| derive_api_key_provider_id(&provider_name, &base_url));
    let auth_path = store
        .data_dir()
        .join("auth")
        .join("api-keys")
        .join(format!("{provider_id}.json"));

    let auth_ref = if let Some(api_key) = api_key {
        if let Some(parent) = auth_path.parent() {
            fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        let auth = serde_json::json!({
            "api_key": api_key,
        });
        let text = serde_json::to_string_pretty(&auth).map_err(|source| {
            CompanionError::InvalidConfig(format!("provider API key serialize failed: {source}"))
        })?;
        fs::write(&auth_path, format!("{text}\n"))
            .map_err(|source| CompanionError::io(&auth_path, source))?;
        Some(format!("file:{}", auth_path.display()))
    } else {
        env_var.as_ref().map(|value| format!("env:{value}"))
    };
    let direct_auth_ref = env_var.as_ref().map(|value| format!("env:{value}"));

    let mut model_map = BTreeMap::new();
    if let Some(model) = model {
        model_map.insert(model.clone(), model);
    }
    let existed = store.load()?.providers.contains_key(&provider_id);
    let provider = add_provider(
        store,
        ProviderUpsert {
            id: provider_id,
            name: provider_name.clone(),
            kind,
            base_url,
            auth_ref,
            direct_auth_ref,
            model_map,
            priority: 100,
            enabled: true,
            refresh_interval_seconds: refresh_interval_seconds
                .unwrap_or_else(default_refresh_interval_seconds),
            account: Some(ProviderAccountInfo {
                display_name: Some(provider_name.clone()),
                email: account_email,
                subscription_type: Some("API Key".to_string()),
                subscription_status: Some("待检查".to_string()),
                ..ProviderAccountInfo::default()
            }),
        },
    )?;

    Ok(ProviderImportOutcome {
        provider,
        import_kind: "api_key".to_string(),
        account_id: "api_key".to_string(),
        auth_path,
        created: !existed,
        message: if existed {
            "已更新 API Key provider".to_string()
        } else {
            "已导入 API Key provider".to_string()
        },
    })
}

fn import_api_key_provider_from_json(
    store: &ConfigStore,
    value: &serde_json::Value,
    explicit_provider_id: Option<String>,
    explicit_provider_name: Option<String>,
) -> Result<ProviderImportOutcome> {
    let api_key = extract_api_key(value).ok_or_else(|| {
        CompanionError::InvalidConfig("API Key JSON 缺少 OPENAI_API_KEY".to_string())
    })?;
    if looks_like_http_url(&api_key) {
        return Err(CompanionError::InvalidConfig(
            "API Key 不能是 URL，请检查 JSON 字段是否填反".to_string(),
        ));
    }
    let base_url = extract_api_base_url(value).unwrap_or_else(default_openai_api_base_url);
    if !looks_like_http_url(&base_url) {
        return Err(CompanionError::InvalidConfig(format!(
            "API Key JSON 的 api_base_url 无效: {base_url}"
        )));
    }
    let provider_name = explicit_provider_name
        .as_deref()
        .and_then(normalize_non_empty)
        .or_else(|| pick_string(value, &[&["api_provider_name"], &["apiProviderName"]]))
        .or_else(|| pick_string(value, &[&["provider_name"], &["providerName"], &["name"]]))
        .or_else(|| Some(provider_name_from_base_url(Some(&base_url))))
        .unwrap_or_else(|| "OpenAI API Key".to_string());
    let provider_id = explicit_provider_id
        .or_else(|| pick_string(value, &[&["api_provider_id"], &["apiProviderId"]]));
    let email = pick_string(value, &[&["email"], &["account", "email"]]);
    let model = extract_model(value);
    let kind = infer_api_key_provider_kind(value, &base_url);

    import_api_key_provider_with_metadata(
        store,
        provider_id,
        provider_name,
        kind,
        base_url,
        api_key,
        None,
        model,
        None,
        email,
    )
}

fn infer_api_key_provider_kind(value: &serde_json::Value, base_url: &str) -> ProviderKind {
    let provider_hint = [
        pick_string(value, &[&["api_provider_id"], &["apiProviderId"]]),
        pick_string(value, &[&["api_provider_name"], &["apiProviderName"]]),
        pick_string(value, &[&["provider_name"], &["providerName"], &["name"]]),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();
    let base_url = base_url.trim().to_ascii_lowercase();

    if provider_hint.contains("new_api")
        || provider_hint.contains("new-api")
        || provider_hint.contains("one-api")
        || !base_url.starts_with("https://api.openai.com/")
    {
        ProviderKind::RelayProvider
    } else {
        ProviderKind::OpenAiCompatible
    }
}

pub fn import_local_codex_provider(
    store: &ConfigStore,
    codex_dir: Option<PathBuf>,
) -> Result<ProviderImportOutcome> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    let auth_path = codex_dir.join("auth.json");
    if !auth_path.exists() {
        return Err(CompanionError::InvalidConfig(format!(
            "未找到 Codex auth.json: {}",
            auth_path.display()
        )));
    }
    let text =
        fs::read_to_string(&auth_path).map_err(|source| CompanionError::io(&auth_path, source))?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|source| CompanionError::json(&auth_path, source))?;
    let config_provider = read_codex_provider_config(&codex_dir);
    if is_auth_mode_api_key(&value) {
        let api_key = extract_api_key(&value).ok_or_else(|| {
            CompanionError::InvalidConfig("auth.json 缺少 OPENAI_API_KEY".to_string())
        })?;
        return import_api_key_provider(
            store,
            config_provider.provider_name.clone().unwrap_or_else(|| {
                provider_name_from_base_url(config_provider.base_url.as_deref())
            }),
            ProviderKind::OpenAiCompatible,
            config_provider
                .base_url
                .clone()
                .unwrap_or_else(default_openai_api_base_url),
            api_key,
            config_provider.api_key_env_var.clone(),
            config_provider.model.clone(),
            None,
        );
    }

    if value.get("tokens").is_some() {
        return import_provider_json(store, &text, None, None);
    }

    if let Some(api_key) = extract_api_key(&value) {
        return import_api_key_provider(
            store,
            config_provider.provider_name.clone().unwrap_or_else(|| {
                provider_name_from_base_url(config_provider.base_url.as_deref())
            }),
            ProviderKind::OpenAiCompatible,
            config_provider
                .base_url
                .clone()
                .unwrap_or_else(default_openai_api_base_url),
            api_key,
            config_provider.api_key_env_var.clone(),
            config_provider.model.clone(),
            None,
        );
    }

    Err(CompanionError::InvalidConfig(
        "auth.json 缺少可导入的 OAuth tokens 或 API key".to_string(),
    ))
}

pub fn parse_provider_import_draft(
    value: &serde_json::Value,
    explicit_provider_id: Option<&str>,
    explicit_provider_name: Option<&str>,
) -> Result<ProviderImportDraft> {
    let auth = extract_codex_oauth_auth(value).ok_or_else(unsupported_import_error)?;
    let account_id = extract_oauth_account_id(value, &auth).unwrap_or_else(|| {
        format!(
            "openai_account_{}",
            stable_hash(&auth.to_string())
                .chars()
                .take(8)
                .collect::<String>()
        )
    });
    let provider_name = explicit_provider_name
        .and_then(normalize_non_empty)
        .or_else(|| extract_oauth_account_name(&auth))
        .or_else(|| extract_provider_name(value))
        .unwrap_or_else(|| "Codex 官方账号".to_string());
    let provider_id = explicit_provider_id
        .and_then(sanitize_provider_id)
        .unwrap_or_else(|| derive_oauth_provider_id(&provider_name, &account_id));
    let model = extract_model(value);

    Ok(ProviderImportDraft {
        provider_id: provider_id.clone(),
        provider_name,
        import_kind: "openai_account".to_string(),
        base_url: OFFICIAL_CODEX_BASE_URL.to_string(),
        auth_ref: format!("file:<companion-data-dir>/auth/accounts/{provider_id}.json"),
        account_id,
        model,
    })
}

fn unsupported_import_error() -> CompanionError {
    CompanionError::InvalidConfig(
        "仅支持 Codex Companion/CPA/sub2api 的 Codex OAuth 或 API Key 账号 JSON".to_string(),
    )
}

fn extract_codex_oauth_auth(value: &serde_json::Value) -> Option<serde_json::Value> {
    let candidate =
        if let Some(accounts) = value.get("accounts").and_then(serde_json::Value::as_array) {
            accounts
                .iter()
                .find(|account| {
                    account
                        .get("platform")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|platform| platform.eq_ignore_ascii_case("openai"))
                        && account
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(|kind| kind.eq_ignore_ascii_case("oauth"))
                })
                .unwrap_or_else(|| accounts.first().unwrap_or(value))
        } else {
            value
        };

    let credentials = candidate.get("credentials").unwrap_or(candidate);
    let extra = candidate.get("extra").unwrap_or(value);
    let access_token = pick_first_string(
        &[credentials, candidate, value],
        &[
            &["access_token"],
            &["accessToken"],
            &["tokens", "access_token"],
            &["tokens", "accessToken"],
            &["token"],
        ],
    );
    let id_token = pick_first_string(
        &[credentials, candidate, value],
        &[
            &["id_token"],
            &["idToken"],
            &["tokens", "id_token"],
            &["tokens", "idToken"],
        ],
    );
    let session_token = pick_first_string(
        &[credentials, candidate, value],
        &[
            &["session_token"],
            &["sessionToken"],
            &["tokens", "session_token"],
            &["tokens", "sessionToken"],
        ],
    );
    let refresh_token = pick_first_string(
        &[credentials, candidate, value],
        &[
            &["refresh_token"],
            &["refreshToken"],
            &["tokens", "refresh_token"],
            &["tokens", "refreshToken"],
        ],
    );

    if access_token.is_none()
        && id_token.is_none()
        && session_token.is_none()
        && refresh_token.is_none()
    {
        return None;
    }

    let account_id = pick_first_string(
        &[credentials, extra, candidate, value],
        &[
            &["chatgpt_account_id"],
            &["account_id"],
            &["tokens", "chatgpt_account_id"],
            &["tokens", "account_id"],
            &["workspace_id"],
            &["chatgpt_user_id"],
        ],
    );
    let email = pick_first_string(
        &[credentials, extra, candidate, value],
        &[
            &["email"],
            &["name"],
            &["tokens", "email"],
            &["tokens", "name"],
        ],
    );
    let name = pick_first_string(
        &[credentials, extra, candidate, value],
        &[
            &["name"],
            &["email"],
            &["tokens", "name"],
            &["tokens", "email"],
        ],
    );
    let plan_type = pick_first_string(
        &[credentials, extra, candidate, value],
        &[
            &["chatgpt_plan_type"],
            &["plan_type"],
            &["tokens", "chatgpt_plan_type"],
            &["tokens", "plan_type"],
        ],
    );
    let expired = pick_first_string(
        &[credentials, extra, candidate, value],
        &[&["expired"], &["expires_at"], &["expiresAt"]],
    );
    let last_refresh = pick_first_string(
        &[credentials, extra, candidate, value],
        &[&["last_refresh"], &["lastRefresh"]],
    );

    let mut tokens = serde_json::Map::new();
    insert_optional_string(&mut tokens, "access_token", access_token);
    insert_optional_string(&mut tokens, "id_token", id_token);
    insert_optional_string(&mut tokens, "refresh_token", refresh_token);
    insert_optional_string(&mut tokens, "session_token", session_token);
    insert_optional_string(&mut tokens, "account_id", account_id.clone());
    insert_optional_string(&mut tokens, "chatgpt_account_id", account_id);
    insert_optional_string(&mut tokens, "email", email.clone());
    insert_optional_string(&mut tokens, "name", name);
    insert_optional_string(&mut tokens, "plan_type", plan_type);
    insert_optional_string(&mut tokens, "expired", expired.clone());
    insert_optional_string(&mut tokens, "last_refresh", last_refresh.clone());

    let mut auth = serde_json::Map::new();
    auth.insert("OPENAI_API_KEY".to_string(), serde_json::Value::Null);
    auth.insert("tokens".to_string(), serde_json::Value::Object(tokens));
    insert_optional_string(&mut auth, "expired", expired);
    insert_optional_string(&mut auth, "last_refresh", last_refresh);
    Some(serde_json::Value::Object(auth))
}

#[derive(Debug, Default)]
struct LocalCodexProviderConfig {
    base_url: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
    api_key_env_var: Option<String>,
}

fn read_codex_provider_config(codex_dir: &Path) -> LocalCodexProviderConfig {
    let config_path = codex_dir.join("config.toml");
    let Ok(text) = fs::read_to_string(&config_path) else {
        return LocalCodexProviderConfig::default();
    };
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return LocalCodexProviderConfig::default();
    };
    let model = doc
        .get("model")
        .and_then(Item::as_str)
        .map(ToOwned::to_owned);
    let provider_id = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .filter(|value| *value != COMPANION_PROVIDER_ID)
        .map(ToOwned::to_owned);
    let Some(provider_id_ref) = provider_id.as_deref() else {
        return LocalCodexProviderConfig {
            model,
            ..LocalCodexProviderConfig::default()
        };
    };
    let provider = doc
        .get("model_providers")
        .and_then(|item| item.get(provider_id_ref));
    LocalCodexProviderConfig {
        base_url: provider
            .and_then(|item| item.get("base_url"))
            .and_then(Item::as_str)
            .and_then(normalize_non_empty),
        provider_name: provider
            .and_then(|item| item.get("name"))
            .and_then(Item::as_str)
            .and_then(normalize_non_empty),
        api_key_env_var: provider
            .and_then(|item| item.get("api_key_env_var"))
            .and_then(Item::as_str)
            .and_then(normalize_non_empty),
        model,
    }
}

fn is_auth_mode_api_key(value: &serde_json::Value) -> bool {
    pick_string(value, &[&["auth_mode"], &["authMode"]]).is_some_and(|mode| {
        mode.eq_ignore_ascii_case("apikey") || mode.eq_ignore_ascii_case("api_key")
    })
}

fn extract_api_key(value: &serde_json::Value) -> Option<String> {
    pick_string(
        value,
        &[
            &["OPENAI_API_KEY"],
            &["openai_api_key"],
            &["api_key"],
            &["apiKey"],
            &["credentials", "api_key"],
            &["tokens", "api_key"],
        ],
    )
}

fn extract_api_base_url(value: &serde_json::Value) -> Option<String> {
    pick_string(
        value,
        &[
            &["api_base_url"],
            &["apiBaseUrl"],
            &["base_url"],
            &["baseUrl"],
            &["credentials", "api_base_url"],
            &["credentials", "base_url"],
        ],
    )
    .map(|value| value.trim_end_matches('/').to_string())
}

fn looks_like_http_url(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn default_openai_api_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn provider_name_from_base_url(base_url: Option<&str>) -> String {
    let Some(base_url) = base_url else {
        return "OpenAI API Key".to_string();
    };
    base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .split('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("OpenAI API Key")
        .to_string()
}

fn extract_provider_account_info(
    value: &serde_json::Value,
    auth: &serde_json::Value,
) -> ProviderAccountInfo {
    let candidate =
        if let Some(accounts) = value.get("accounts").and_then(serde_json::Value::as_array) {
            accounts
                .iter()
                .find(|account| {
                    account
                        .get("platform")
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|platform| platform.eq_ignore_ascii_case("openai"))
                        && account
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(|kind| kind.eq_ignore_ascii_case("oauth"))
                })
                .unwrap_or_else(|| accounts.first().unwrap_or(value))
        } else {
            value
        };
    let credentials = candidate.get("credentials").unwrap_or(candidate);
    let extra = candidate.get("extra").unwrap_or(value);
    let tokens = auth.get("tokens").unwrap_or(&serde_json::Value::Null);
    let sources = [credentials, extra, candidate, value, tokens, auth];

    let email = pick_first_string(&sources, &[&["email"], &["account", "email"]]);
    let display_name = pick_first_string(
        &sources,
        &[
            &["display_name"],
            &["displayName"],
            &["name"],
            &["username"],
            &["account_name"],
            &["accountName"],
            &["email"],
        ],
    );
    let team_name = pick_first_string(
        &sources,
        &[
            &["team_name"],
            &["teamName"],
            &["team", "name"],
            &["workspace_name"],
            &["workspaceName"],
            &["workspace", "name"],
            &["organization_name"],
            &["organizationName"],
            &["org_name"],
            &["orgName"],
        ],
    );
    let account_id = pick_first_string(
        &sources,
        &[
            &["chatgpt_account_id"],
            &["account_id"],
            &["accountId"],
            &["workspace_id"],
        ],
    );
    let user_id = pick_first_string(
        &sources,
        &[
            &["chatgpt_user_id"],
            &["user_id"],
            &["userId"],
            &["user", "id"],
        ],
    );
    let subscription_type = pick_first_string(
        &sources,
        &[
            &["subscription_type"],
            &["subscriptionType"],
            &["plan_type"],
            &["planType"],
            &["auth_file_plan_type"],
            &["authFilePlanType"],
            &["chatgpt_plan_type"],
            &["account_plan"],
            &["accountPlan"],
            &["sku"],
        ],
    )
    .map(|value| value.to_ascii_uppercase());
    let subscription_status = pick_first_string(
        &sources,
        &[
            &["subscription_status"],
            &["subscriptionStatus"],
            &["status"],
            &["current"],
        ],
    );
    let quota_label = pick_first_string(
        &sources,
        &[
            &["quota_label"],
            &["quotaLabel"],
            &["quota", "label"],
            &["usage_label"],
            &["usageLabel"],
            &["window_label"],
            &["windowLabel"],
            &["model"],
        ],
    );
    let quota_percent = pick_first_number(
        &sources,
        &[
            &["quota_percent"],
            &["quotaPercent"],
            &["quota", "percent"],
            &["usage_percent"],
            &["usagePercent"],
            &["usage", "percent"],
            &["percent"],
            &["used_percent"],
            &["usedPercent"],
        ],
    )
    .map(normalize_percent);
    let quota_reset_at = pick_first_string(
        &sources,
        &[
            &["quota_reset_at"],
            &["quotaResetAt"],
            &["quota", "reset_at"],
            &["quota", "resetAt"],
            &["reset_at"],
            &["resetAt"],
            &["reset_time"],
            &["resetTime"],
        ],
    );
    let valid_until = pick_first_string(
        &sources,
        &[
            &["valid_until"],
            &["validUntil"],
            &["expires_at"],
            &["expiresAt"],
            &["expired"],
            &["subscription_expires_at"],
            &["subscriptionExpiresAt"],
            &["subscription_active_until"],
            &["subscriptionActiveUntil"],
            &["chatgpt_subscription_active_until"],
            &["active_until"],
            &["activeUntil"],
            &["entitlement", "subscription_active_until"],
            &["entitlement", "expires_at"],
        ],
    );
    let quota_windows = extract_quota_windows(&sources);
    let last_refresh_at = pick_first_string(
        &sources,
        &[
            &["last_refresh"],
            &["lastRefresh"],
            &["last_refresh_at"],
            &["lastRefreshAt"],
            &["refreshed_at"],
            &["refreshedAt"],
            &["updated_at"],
            &["updatedAt"],
        ],
    );

    ProviderAccountInfo {
        display_name,
        email,
        team_name,
        account_id,
        user_id,
        subscription_type,
        subscription_status,
        quota_label,
        quota_percent,
        quota_reset_at,
        quota_windows,
        usage_total: None,
        usage_used: None,
        usage_available: None,
        valid_until,
        last_refresh_at,
    }
}

fn extract_oauth_account_id(
    source: &serde_json::Value,
    auth: &serde_json::Value,
) -> Option<String> {
    pick_first_string(
        &[
            auth,
            auth.get("tokens").unwrap_or(&serde_json::Value::Null),
            source,
        ],
        &[
            &["chatgpt_account_id"],
            &["account_id"],
            &["tokens", "chatgpt_account_id"],
            &["tokens", "account_id"],
            &["credentials", "chatgpt_account_id"],
            &["credentials", "account_id"],
        ],
    )
}

fn extract_oauth_account_name(auth: &serde_json::Value) -> Option<String> {
    pick_first_string(
        &[auth, auth.get("tokens").unwrap_or(&serde_json::Value::Null)],
        &[
            &["name"],
            &["email"],
            &["tokens", "name"],
            &["tokens", "email"],
        ],
    )
}

fn extract_provider_name(value: &serde_json::Value) -> Option<String> {
    pick_string(
        value,
        &[
            &["providerName"],
            &["provider_name"],
            &["name"],
            &["label"],
            &["provider", "name"],
        ],
    )
}

fn extract_model(value: &serde_json::Value) -> Option<String> {
    pick_string(value, &[&["model"], &["defaultModel"], &["default_model"]])
}

fn pick_first_string(values: &[&serde_json::Value], paths: &[&[&str]]) -> Option<String> {
    values.iter().find_map(|value| pick_string(value, paths))
}

fn pick_first_number(values: &[&serde_json::Value], paths: &[&[&str]]) -> Option<f64> {
    values.iter().find_map(|value| pick_number(value, paths))
}

fn extract_quota_windows(sources: &[&serde_json::Value]) -> Vec<ProviderQuotaWindow> {
    [
        extract_quota_window(
            sources,
            "5h",
            &[
                &["hourly_percentage"][..],
                &["hourlyPercentage"][..],
                &["quota", "hourly_percentage"][..],
                &["quota", "hourlyPercentage"][..],
                &["primary_window", "remaining_percent"][..],
                &["primaryWindow", "remainingPercent"][..],
            ],
            &[
                &["hourly_reset_time"][..],
                &["hourlyResetTime"][..],
                &["quota", "hourly_reset_time"][..],
                &["quota", "hourlyResetTime"][..],
                &["primary_window", "reset_at"][..],
                &["primaryWindow", "resetAt"][..],
            ],
            &[
                &["hourly_window_minutes"][..],
                &["hourlyWindowMinutes"][..],
                &["quota", "hourly_window_minutes"][..],
                &["quota", "hourlyWindowMinutes"][..],
                &["primary_window", "window_minutes"][..],
                &["primaryWindow", "windowMinutes"][..],
            ],
        ),
        extract_quota_window(
            sources,
            "Week",
            &[
                &["weekly_percentage"][..],
                &["weeklyPercentage"][..],
                &["quota", "weekly_percentage"][..],
                &["quota", "weeklyPercentage"][..],
                &["secondary_window", "remaining_percent"][..],
                &["secondaryWindow", "remainingPercent"][..],
            ],
            &[
                &["weekly_reset_time"][..],
                &["weeklyResetTime"][..],
                &["quota", "weekly_reset_time"][..],
                &["quota", "weeklyResetTime"][..],
                &["secondary_window", "reset_at"][..],
                &["secondaryWindow", "resetAt"][..],
            ],
            &[
                &["weekly_window_minutes"][..],
                &["weeklyWindowMinutes"][..],
                &["quota", "weekly_window_minutes"][..],
                &["quota", "weeklyWindowMinutes"][..],
                &["secondary_window", "window_minutes"][..],
                &["secondaryWindow", "windowMinutes"][..],
            ],
        ),
        extract_quota_window(
            sources,
            "Code Review",
            &[
                &["code_review_percentage"][..],
                &["codeReviewPercentage"][..],
                &["quota", "code_review_percentage"][..],
                &["quota", "codeReviewPercentage"][..],
                &["code_review", "remaining_percent"][..],
                &["codeReview", "remainingPercent"][..],
            ],
            &[
                &["code_review_reset_time"][..],
                &["codeReviewResetTime"][..],
                &["quota", "code_review_reset_time"][..],
                &["quota", "codeReviewResetTime"][..],
                &["code_review", "reset_at"][..],
                &["codeReview", "resetAt"][..],
            ],
            &[
                &["code_review_window_minutes"][..],
                &["codeReviewWindowMinutes"][..],
                &["quota", "code_review_window_minutes"][..],
                &["quota", "codeReviewWindowMinutes"][..],
                &["code_review", "window_minutes"][..],
                &["codeReview", "windowMinutes"][..],
            ],
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn extract_quota_window(
    sources: &[&serde_json::Value],
    label: &str,
    percent_paths: &[&[&str]],
    reset_paths: &[&[&str]],
    window_paths: &[&[&str]],
) -> Option<ProviderQuotaWindow> {
    let remaining_percent = pick_first_number(sources, percent_paths).map(normalize_percent)?;
    let window_minutes = pick_first_number(sources, window_paths).map(|value| value.round() as i64);
    Some(ProviderQuotaWindow {
        label: label.to_string(),
        remaining_percent,
        reset_at: pick_first_string(sources, reset_paths),
        window_minutes,
    })
}

fn pick_number(value: &serde_json::Value, paths: &[&[&str]]) -> Option<f64> {
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
            if let Some(number) = value_to_number(cursor) {
                return Some(number);
            }
        }
    }
    match value {
        serde_json::Value::Object(map) => map.values().find_map(|child| pick_number(child, paths)),
        serde_json::Value::Array(items) => items.iter().find_map(|item| pick_number(item, paths)),
        _ => None,
    }
}

fn value_to_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().trim_end_matches('%').parse::<f64>().ok(),
        _ => None,
    }
}

fn normalize_percent(value: f64) -> f64 {
    if (0.0..=1.0).contains(&value) {
        value * 100.0
    } else {
        value
    }
    .clamp(0.0, 100.0)
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
            if let Some(text) = cursor.as_str().and_then(normalize_non_empty) {
                return Some(text);
            }
        }
    }
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values() {
                if let Some(text) = pick_string(child, paths) {
                    return Some(text);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items.iter().find_map(|item| pick_string(item, paths)),
        _ => None,
    }
}

fn insert_optional_string(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        map.insert(key.to_string(), serde_json::Value::String(value));
    }
}

fn normalize_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn derive_oauth_provider_id(provider_name: &str, account_id: &str) -> String {
    let name = sanitize_provider_id(provider_name).unwrap_or_else(|| "chatgpt".to_string());
    format!(
        "codex_openai_{}_{}",
        name,
        stable_hash(account_id).chars().take(8).collect::<String>()
    )
}

fn derive_api_key_provider_id(provider_name: &str, base_url: &str) -> String {
    let name = sanitize_provider_id(provider_name).unwrap_or_else(|| "provider".to_string());
    format!(
        "{}_{}",
        name,
        stable_hash(base_url).chars().take(8).collect::<String>()
    )
}

fn sanitize_provider_id(raw: &str) -> Option<String> {
    let mut output = String::new();
    let mut previous_separator = false;
    for ch in raw.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            previous_separator = false;
            ch.to_ascii_lowercase()
        } else if matches!(ch, '-' | '_' | '.') {
            if previous_separator {
                continue;
            }
            previous_separator = true;
            '_'
        } else {
            if previous_separator {
                continue;
            }
            previous_separator = true;
            '_'
        };
        output.push(mapped);
    }
    let output = output.trim_matches('_').to_string();
    if output.is_empty() {
        return None;
    }
    if output
        .chars()
        .next()
        .map(|ch| ch.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        Some(output)
    } else {
        Some(format!("provider_{output}"))
    }
}

fn stable_hash(value: impl AsRef<str>) -> String {
    let mut hasher = DefaultHasher::new();
    value.as_ref().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpa_codex_oauth_json() {
        let value = serde_json::json!({
            "type": "codex",
            "account_id": "00000000-0000-4000-9000-000000000000",
            "chatgpt_account_id": "00000000-0000-4000-9000-000000000000",
            "email": "mark@example.com",
            "name": "mark@example.com",
            "plan_type": "plus",
            "id_token": "id-token",
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "session_token": "session-token"
        });
        let draft = parse_provider_import_draft(&value, None, None).expect("draft");
        assert_eq!(draft.import_kind, "openai_account");
        assert_eq!(draft.provider_name, "mark@example.com");
        assert_eq!(draft.account_id, "00000000-0000-4000-9000-000000000000");
        assert_eq!(draft.base_url, OFFICIAL_CODEX_BASE_URL);
        assert_eq!(draft.model, None);
    }

    #[test]
    fn preserves_explicit_model_without_inventing_an_official_default() {
        let value = serde_json::json!({
            "access_token": "access-token",
            "account_id": "account-id",
            "model": "current-codex-model"
        });

        let draft = parse_provider_import_draft(&value, None, None).expect("draft");

        assert_eq!(draft.model.as_deref(), Some("current-codex-model"));
    }

    #[test]
    fn parses_sub2api_oauth_export_account() {
        let value = serde_json::json!({
            "providerName": "Sub2API Export",
            "accounts": [{
                "name": "mark@example.com",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "access-token",
                    "account_id": "account-id",
                    "chatgpt_account_id": "chatgpt-account-id",
                    "id_token": "id-token",
                    "email": "mark@example.com",
                    "plan_type": "plus"
                },
                "extra": {
                    "last_refresh": "2026-06-06T09:11:35.028Z"
                }
            }]
        });
        let draft = parse_provider_import_draft(&value, None, None).expect("draft");
        assert_eq!(draft.account_id, "chatgpt-account-id");
        assert_eq!(draft.provider_name, "mark@example.com");
    }

    #[test]
    fn imports_each_account_from_multi_account_json() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "providerName": "Sub2API Export",
            "accounts": [
                {
                    "name": "a@example.com",
                    "platform": "openai",
                    "type": "oauth",
                    "credentials": {
                        "access_token": "access-a",
                        "refresh_token": "refresh-a",
                        "chatgpt_account_id": "account-a"
                    }
                },
                {
                    "name": "b@example.com",
                    "platform": "openai",
                    "type": "oauth",
                    "credentials": {
                        "access_token": "access-b",
                        "refresh_token": "refresh-b",
                        "chatgpt_account_id": "account-b"
                    }
                }
            ]
        });

        let outcomes =
            import_provider_json_many(&store, &value.to_string(), None, None).expect("import");
        let config = store.load().expect("config");

        assert_eq!(outcomes.len(), 2);
        assert_eq!(config.providers.len(), 2);
        assert!(outcomes
            .iter()
            .any(|outcome| outcome.account_id == "account-a"));
        assert!(outcomes
            .iter()
            .any(|outcome| outcome.account_id == "account-b"));
    }

    #[test]
    fn import_persists_account_summary() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "accounts": [{
                "name": "ctf01zv4n73g7@gptteam.ikun.edu.rs",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "access-token",
                    "account_id": "account-id",
                    "chatgpt_account_id": "chatgpt-account-id",
                    "email": "ctf01zv4n73g7@gptteam.ikun.edu.rs",
                    "plan_type": "team"
                },
                "extra": {
                    "team_name": "035611",
                    "user_id": "0870da3c-a509-4340-b33a-30",
                    "quota_label": "5 Week",
                    "hourly_percentage": 60,
                    "hourly_reset_time": "2026-06-08T13:34:00Z",
                    "weekly_percentage": 77,
                    "weekly_reset_time": "2026-07-07T23:04:00Z",
                    "quota_reset_at": "2026-07-07T23:04:00Z",
                    "subscription_active_until": "2026-07-06T13:27:00Z",
                    "last_refresh": "2026-06-07T13:03:00Z"
                }
            }]
        });

        let outcome = import_provider_json(&store, &value.to_string(), None, None).expect("import");
        let account = outcome.provider.account.expect("account summary");
        assert_eq!(
            account.email.as_deref(),
            Some("ctf01zv4n73g7@gptteam.ikun.edu.rs")
        );
        assert_eq!(account.account_id.as_deref(), Some("chatgpt-account-id"));
        assert_eq!(account.team_name.as_deref(), Some("035611"));
        assert_eq!(account.subscription_type.as_deref(), Some("TEAM"));
        assert_eq!(account.quota_windows.len(), 2);
        assert_eq!(account.quota_windows[0].remaining_percent, 60.0);
        assert_eq!(account.quota_windows[1].remaining_percent, 77.0);
        assert_eq!(
            account.quota_reset_at.as_deref(),
            Some("2026-07-07T23:04:00Z")
        );
        assert_eq!(account.valid_until.as_deref(), Some("2026-07-06T13:27:00Z"));
    }

    #[test]
    fn imports_local_codex_oauth_auth_json() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("companion").join("config.json"));
        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        fs::write(
            codex_dir.join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "access_token": "access-token",
                    "id_token": "id-token",
                    "refresh_token": "refresh-token",
                    "chatgpt_account_id": "local-account",
                    "email": "local@example.com",
                    "plan_type": "plus"
                }
            })
            .to_string(),
        )
        .expect("auth");

        let outcome = import_local_codex_provider(&store, Some(codex_dir)).expect("import");
        assert_eq!(outcome.provider.kind, ProviderKind::OfficialCodex);
        assert_eq!(
            outcome
                .provider
                .account
                .and_then(|account| account.account_id),
            Some("local-account".to_string())
        );
    }

    #[test]
    fn imports_local_codex_api_key_with_config_provider() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("companion").join("config.json"));
        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        fs::write(
            codex_dir.join("auth.json"),
            serde_json::json!({
                "auth_mode": "apikey",
                "OPENAI_API_KEY": "sk-local"
            })
            .to_string(),
        )
        .expect("auth");
        fs::write(
            codex_dir.join("config.toml"),
            r#"
model = "deepseek-chat"
model_provider = "deepseek"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
"#,
        )
        .expect("config");

        let outcome = import_local_codex_provider(&store, Some(codex_dir)).expect("import");
        assert_eq!(outcome.provider.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(outcome.provider.name, "DeepSeek");
        assert_eq!(outcome.provider.base_url, "https://api.deepseek.com/v1");
        assert!(outcome.provider.model_map.contains_key("deepseek-chat"));
    }

    #[test]
    fn imports_api_key_json_with_provider_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": "sk-test",
            "email": "api-key-1234",
            "api_base_url": "https://sub2api.example.com/v1/",
            "api_provider_id": "sub2api_test",
            "api_provider_name": "Sub2API Test"
        });
        let outcome = import_provider_json(&store, &value.to_string(), None, None).expect("import");
        assert_eq!(outcome.provider.id, "sub2api_test");
        assert_eq!(outcome.provider.name, "Sub2API Test");
        assert_eq!(outcome.provider.kind, ProviderKind::RelayProvider);
        assert_eq!(outcome.provider.base_url, "https://sub2api.example.com/v1");
        assert_eq!(
            outcome.provider.account.unwrap().email.as_deref(),
            Some("api-key-1234")
        );
    }

    #[test]
    fn imports_api_key_json_array() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!([
            {
                "auth_mode": "apikey",
                "OPENAI_API_KEY": "sk-a",
                "api_base_url": "https://a.example.com/v1",
                "api_provider_id": "provider_a",
                "api_provider_name": "Provider A"
            },
            {
                "auth_mode": "apikey",
                "OPENAI_API_KEY": "sk-b",
                "api_base_url": "https://b.example.com/v1",
                "api_provider_id": "provider_b",
                "api_provider_name": "Provider B"
            }
        ]);
        let outcomes =
            import_provider_json_many(&store, &value.to_string(), None, None).expect("import");
        assert_eq!(outcomes.len(), 2);
        let config = store.load().expect("config");
        assert!(config.providers.contains_key("provider_a"));
        assert!(config.providers.contains_key("provider_b"));
    }
}
