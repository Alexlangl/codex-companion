use crate::persist_with_private_auth_file;
use crate::registry::add_provider;
use crate::types::{
    ProviderImportBatchReport, ProviderImportDraft, ProviderImportFailure, ProviderImportOutcome,
    ProviderImportReviewItem, ProviderImportReviewReport, ProviderUpsert, OFFICIAL_CODEX_BASE_URL,
};
use base64::{engine::general_purpose, Engine as _};
use codex_companion_core::{
    default_codex_dir, default_refresh_interval_seconds, redact_sensitive_text, CompanionError,
    ConfigStore, ProviderAccountInfo, ProviderImportProgress, ProviderKind, ProviderQuotaWindow,
    Result, COMPANION_PROVIDER_ID,
};
use ed25519_dalek::{pkcs8::DecodePrivateKey, SigningKey};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use toml_edit::{DocumentMut, Item};

enum ProviderImportPlan {
    OAuth {
        draft: ProviderImportDraft,
        auth: serde_json::Value,
        account: ProviderAccountInfo,
    },
    AgentIdentity {
        auth: serde_json::Value,
        account: ProviderAccountInfo,
        account_id: String,
        provider_id: String,
        provider_name: String,
    },
    ApiKey {
        input: ApiKeyProviderImportRequest,
        provider_id: String,
        account_email: Option<String>,
    },
}

pub fn import_provider_json(
    store: &ConfigStore,
    json_text: &str,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<ProviderImportOutcome> {
    let value = serde_json::from_str::<serde_json::Value>(json_text).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON parse failed: {source}"))
    })?;
    let plan = prepare_provider_import(
        store,
        &value,
        provider_id.as_deref(),
        provider_name.as_deref(),
    )?;
    execute_provider_import(store, plan)
}

fn prepare_provider_import(
    store: &ConfigStore,
    value: &serde_json::Value,
    explicit_provider_id: Option<&str>,
    explicit_provider_name: Option<&str>,
) -> Result<ProviderImportPlan> {
    if let Some(auth) = extract_agent_identity_auth(value) {
        validate_agent_identity_auth(&auth)?;
        let account = extract_provider_account_info(value, &auth);
        let account_id = account.account_id.clone().ok_or_else(|| {
            CompanionError::InvalidConfig("Agent Identity 缺少 ChatGPT account id".to_string())
        })?;
        let user_id = account.user_id.as_deref();
        let provider_name = explicit_provider_name
            .and_then(normalize_non_empty)
            .or_else(|| account.email.clone())
            .or_else(|| account.display_name.clone())
            .unwrap_or_else(|| "Codex 官方账号".to_string());
        let existing_provider_id = existing_provider_id_for_identity(store, &account_id, user_id)?;
        let provider_id = explicit_provider_id
            .and_then(sanitize_provider_id)
            .or(existing_provider_id)
            .unwrap_or_else(|| {
                derive_oauth_provider_id(
                    &provider_name,
                    &account_identity_key(&account_id, user_id),
                )
            });
        return Ok(ProviderImportPlan::AgentIdentity {
            auth,
            account,
            account_id,
            provider_id,
            provider_name,
        });
    }
    if is_auth_mode_api_key(value)
        || is_newapi_channel_connection(value)
        || extract_api_key(value).is_some()
    {
        return prepare_api_key_provider_from_json(
            value,
            explicit_provider_id,
            explicit_provider_name,
        );
    }
    let mut draft =
        parse_provider_import_draft(value, explicit_provider_id, explicit_provider_name)?;
    if explicit_provider_id.and_then(normalize_non_empty).is_none() {
        if let Some(existing_id) =
            existing_provider_id_for_identity(store, &draft.account_id, draft.user_id.as_deref())?
        {
            draft.provider_id = existing_id;
        }
    }
    let auth = extract_codex_oauth_auth(value).ok_or_else(unsupported_import_error)?;
    let account = extract_provider_account_info(value, &auth);
    Ok(ProviderImportPlan::OAuth {
        draft,
        auth,
        account,
    })
}

fn execute_provider_import(
    store: &ConfigStore,
    plan: ProviderImportPlan,
) -> Result<ProviderImportOutcome> {
    match plan {
        ProviderImportPlan::OAuth {
            draft,
            auth,
            account,
        } => import_oauth_provider(store, draft, auth, account),
        ProviderImportPlan::AgentIdentity {
            auth,
            account,
            account_id,
            provider_id,
            provider_name,
        } => import_agent_identity_provider(
            store,
            auth,
            account,
            account_id,
            provider_id,
            provider_name,
        ),
        ProviderImportPlan::ApiKey {
            input,
            provider_id,
            account_email,
        } => import_api_key_provider_with_metadata(store, input, Some(provider_id), account_email),
    }
}

fn import_oauth_provider(
    store: &ConfigStore,
    draft: ProviderImportDraft,
    auth: serde_json::Value,
    account: ProviderAccountInfo,
) -> Result<ProviderImportOutcome> {
    let auth_path = store
        .data_dir()
        .join("auth")
        .join("accounts")
        .join(format!("{}.json", draft.provider_id));
    if let Some(parent) = auth_path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }

    let text = serde_json::to_string_pretty(&auth).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON serialize failed: {source}"))
    })?;
    let auth_contents = format!("{text}\n");

    let mut model_map = BTreeMap::new();
    if let Some(model) = draft.model.as_ref() {
        model_map.insert(model.clone(), model.clone());
    }
    let existed = store.load()?.providers.contains_key(&draft.provider_id);
    let provider_input = ProviderUpsert {
        id: draft.provider_id,
        name: draft.provider_name,
        kind: ProviderKind::OfficialCodex,
        base_url: draft.base_url,
        websocket_url: None,
        auth_ref: Some(format!("file:{}", auth_path.display())),
        direct_auth_ref: None,
        model_map,
        priority: 50,
        enabled: true,
        refresh_interval_seconds: default_refresh_interval_seconds(),
        account: Some(account),
    };
    let provider = persist_with_private_auth_file(&auth_path, &auth_contents, || {
        add_provider(store, provider_input)
    })?;

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

fn import_agent_identity_provider(
    store: &ConfigStore,
    auth: serde_json::Value,
    account: ProviderAccountInfo,
    account_id: String,
    provider_id: String,
    provider_name: String,
) -> Result<ProviderImportOutcome> {
    let auth_path = store
        .data_dir()
        .join("auth")
        .join("accounts")
        .join(format!("{provider_id}.json"));
    if let Some(parent) = auth_path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }
    let text = serde_json::to_string_pretty(&auth).map_err(|source| {
        CompanionError::InvalidConfig(format!("Agent Identity serialize failed: {source}"))
    })?;
    let auth_contents = format!("{text}\n");

    let existed = store.load()?.providers.contains_key(&provider_id);
    let provider_input = ProviderUpsert {
        id: provider_id,
        name: provider_name,
        kind: ProviderKind::OfficialCodex,
        base_url: OFFICIAL_CODEX_BASE_URL.to_string(),
        websocket_url: None,
        auth_ref: Some(format!("file:{}", auth_path.display())),
        direct_auth_ref: None,
        model_map: BTreeMap::new(),
        priority: 50,
        enabled: true,
        refresh_interval_seconds: default_refresh_interval_seconds(),
        account: Some(account),
    };
    let provider = persist_with_private_auth_file(&auth_path, &auth_contents, || {
        add_provider(store, provider_input)
    })?;

    Ok(ProviderImportOutcome {
        provider,
        import_kind: "agent_identity".to_string(),
        account_id,
        auth_path,
        created: !existed,
        message: if existed {
            "已更新 Agent Identity provider".to_string()
        } else {
            "已导入 Agent Identity provider".to_string()
        },
    })
}

static PROVIDER_IMPORT_PROGRESS: OnceLock<Mutex<ProviderImportProgress>> = OnceLock::new();

pub fn provider_import_progress() -> ProviderImportProgress {
    PROVIDER_IMPORT_PROGRESS
        .get_or_init(|| Mutex::new(ProviderImportProgress::default()))
        .lock()
        .map(|progress| progress.clone())
        .unwrap_or_default()
}

pub fn import_provider_json_many(
    store: &ConfigStore,
    json_text: &str,
    provider_id: Option<String>,
    provider_name: Option<String>,
    add_to_group_id: Option<String>,
) -> Result<ProviderImportBatchReport> {
    let items = parse_provider_import_items(json_text, provider_id.as_deref())?;

    set_provider_import_progress(ProviderImportProgress {
        active: true,
        total: items.len(),
        started_at: Some(chrono::Utc::now()),
        ..ProviderImportProgress::default()
    });
    let mut report = ProviderImportBatchReport {
        total: items.len(),
        ..ProviderImportBatchReport::default()
    };
    for (index, item) in items.into_iter().enumerate() {
        let label = import_item_label(&item, index);
        update_provider_import_progress(index, &label, report.succeeded.len(), report.failed.len());
        let item_provider_id = (report.total == 1).then(|| provider_id.clone()).flatten();
        match import_provider_json(
            store,
            &item.to_string(),
            item_provider_id,
            provider_name.clone(),
        ) {
            Ok(outcome) => report.succeeded.push(outcome),
            Err(error) => report.failed.push(ProviderImportFailure {
                index,
                label,
                message: redact_sensitive_text(&error.to_string()),
            }),
        }
    }
    if let Some(group_id) = add_to_group_id.as_deref().and_then(normalize_non_empty) {
        report.added_to_group =
            add_imported_providers_to_group(store, &group_id, &report.succeeded)?;
    }
    finish_provider_import_progress(report.succeeded.len(), report.failed.len());
    Ok(report)
}

pub fn review_provider_json_many(
    store: &ConfigStore,
    json_text: &str,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<ProviderImportReviewReport> {
    let items = parse_provider_import_items(json_text, provider_id.as_deref())?;
    let config = store.load()?;
    let mut report = ProviderImportReviewReport {
        total: items.len(),
        ..ProviderImportReviewReport::default()
    };
    let mut reviewed_provider_ids = BTreeSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let label = import_item_label(&item, index);
        let item_provider_id = (report.total == 1)
            .then_some(provider_id.as_deref())
            .flatten();
        match prepare_provider_import(store, &item, item_provider_id, provider_name.as_deref()) {
            Ok(plan) => {
                let provider_id = provider_import_id(&plan);
                let will_overwrite = config.providers.contains_key(provider_id)
                    || !reviewed_provider_ids.insert(provider_id.to_string());
                report.ready.push(provider_import_review_item(
                    &plan,
                    index,
                    label,
                    will_overwrite,
                ));
            }
            Err(error) => report.failed.push(ProviderImportFailure {
                index,
                label,
                message: redact_sensitive_text(&error.to_string()),
            }),
        }
    }
    Ok(report)
}

fn parse_provider_import_items(
    json_text: &str,
    explicit_provider_id: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let value = serde_json::from_str::<serde_json::Value>(json_text).map_err(|source| {
        CompanionError::InvalidConfig(format!("provider JSON parse failed: {source}"))
    })?;
    if let Some(items) = value.as_array().filter(|items| !items.is_empty()) {
        if explicit_provider_id.and_then(normalize_non_empty).is_some() {
            return Err(CompanionError::InvalidConfig(
                "批量 provider JSON 不能同时指定单个 provider id".to_string(),
            ));
        }
        return Ok(items.clone());
    }
    if let Some(accounts) = value
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .filter(|accounts| accounts.len() > 1)
    {
        if explicit_provider_id.and_then(normalize_non_empty).is_some() {
            return Err(CompanionError::InvalidConfig(
                "批量账号 JSON 不能同时指定单个 provider id".to_string(),
            ));
        }
        return Ok(accounts
            .iter()
            .map(|account| {
                let mut item = value.clone();
                if let Some(object) = item.as_object_mut() {
                    object.insert(
                        "accounts".to_string(),
                        serde_json::Value::Array(vec![account.clone()]),
                    );
                }
                item
            })
            .collect());
    }
    Ok(vec![value])
}

fn provider_import_id(plan: &ProviderImportPlan) -> &str {
    match plan {
        ProviderImportPlan::OAuth { draft, .. } => &draft.provider_id,
        ProviderImportPlan::AgentIdentity { provider_id, .. }
        | ProviderImportPlan::ApiKey { provider_id, .. } => provider_id,
    }
}

fn provider_import_review_item(
    plan: &ProviderImportPlan,
    index: usize,
    label: String,
    will_overwrite: bool,
) -> ProviderImportReviewItem {
    match plan {
        ProviderImportPlan::OAuth { draft, .. } => ProviderImportReviewItem {
            index,
            label,
            provider_id: draft.provider_id.clone(),
            provider_name: redact_sensitive_text(&draft.provider_name),
            provider_kind: ProviderKind::OfficialCodex,
            import_kind: draft.import_kind.clone(),
            credential_kind: "OAuth tokens".to_string(),
            base_url: redact_sensitive_text(&draft.base_url),
            websocket_url: None,
            model: draft.model.as_deref().map(redact_sensitive_text),
            will_overwrite,
        },
        ProviderImportPlan::AgentIdentity {
            provider_id,
            provider_name,
            ..
        } => ProviderImportReviewItem {
            index,
            label,
            provider_id: provider_id.clone(),
            provider_name: redact_sensitive_text(provider_name),
            provider_kind: ProviderKind::OfficialCodex,
            import_kind: "agent_identity".to_string(),
            credential_kind: "Agent Identity 私钥".to_string(),
            base_url: OFFICIAL_CODEX_BASE_URL.to_string(),
            websocket_url: None,
            model: None,
            will_overwrite,
        },
        ProviderImportPlan::ApiKey {
            input, provider_id, ..
        } => ProviderImportReviewItem {
            index,
            label,
            provider_id: provider_id.clone(),
            provider_name: redact_sensitive_text(&input.provider_name),
            provider_kind: input.kind.clone(),
            import_kind: "api_key".to_string(),
            credential_kind: "API Key".to_string(),
            base_url: redact_sensitive_text(&input.base_url),
            websocket_url: input.websocket_url.as_deref().map(redact_sensitive_text),
            model: input.model.as_deref().map(redact_sensitive_text),
            will_overwrite,
        },
    }
}

fn import_item_label(value: &serde_json::Value, index: usize) -> String {
    pick_string(
        value,
        &[
            &["email"],
            &["name"],
            &["chatgpt_user_id"],
            &["user_id"],
            &["chatgpt_account_id"],
            &["account_id"],
            &["credentials", "email"],
        ],
    )
    .map(|label| redact_sensitive_text(&label))
    .unwrap_or_else(|| format!("账号 {}", index + 1))
}

fn add_imported_providers_to_group(
    store: &ConfigStore,
    group_id: &str,
    outcomes: &[ProviderImportOutcome],
) -> Result<Vec<String>> {
    let provider_ids = outcomes
        .iter()
        .map(|outcome| outcome.provider.id.clone())
        .collect::<Vec<_>>();
    store.update(|config| {
        let group = config
            .groups
            .get_mut(group_id)
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {group_id}")))?;
        let mut added = Vec::new();
        for provider_id in &provider_ids {
            if !group.provider_order.contains(provider_id) {
                group.provider_order.push(provider_id.clone());
                added.push(provider_id.clone());
            }
        }
        Ok(added)
    })
}

fn set_provider_import_progress(progress: ProviderImportProgress) {
    if let Ok(mut current) = PROVIDER_IMPORT_PROGRESS
        .get_or_init(|| Mutex::new(ProviderImportProgress::default()))
        .lock()
    {
        *current = progress;
    }
}

fn update_provider_import_progress(completed: usize, label: &str, succeeded: usize, failed: usize) {
    if let Ok(mut progress) = PROVIDER_IMPORT_PROGRESS
        .get_or_init(|| Mutex::new(ProviderImportProgress::default()))
        .lock()
    {
        progress.completed = completed;
        progress.current_label = Some(label.to_string());
        progress.succeeded = succeeded;
        progress.failed = failed;
    }
}

fn finish_provider_import_progress(succeeded: usize, failed: usize) {
    if let Ok(mut progress) = PROVIDER_IMPORT_PROGRESS
        .get_or_init(|| Mutex::new(ProviderImportProgress::default()))
        .lock()
    {
        progress.active = false;
        progress.completed = progress.total;
        progress.current_label = None;
        progress.succeeded = succeeded;
        progress.failed = failed;
        progress.finished_at = Some(chrono::Utc::now());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyProviderImportRequest {
    pub provider_name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub websocket_url: Option<String>,
    pub api_key: String,
    pub env_var: Option<String>,
    pub model: Option<String>,
    pub refresh_interval_seconds: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
pub fn import_api_key_provider(
    store: &ConfigStore,
    provider_name: String,
    kind: ProviderKind,
    base_url: String,
    websocket_url: Option<String>,
    api_key: String,
    env_var: Option<String>,
    model: Option<String>,
    refresh_interval_seconds: Option<u64>,
) -> Result<ProviderImportOutcome> {
    import_api_key_provider_request(
        store,
        ApiKeyProviderImportRequest {
            provider_name,
            kind,
            base_url,
            websocket_url,
            api_key,
            env_var,
            model,
            refresh_interval_seconds,
        },
    )
}

pub fn import_api_key_provider_request(
    store: &ConfigStore,
    input: ApiKeyProviderImportRequest,
) -> Result<ProviderImportOutcome> {
    import_api_key_provider_with_metadata(store, input, None, None)
}

fn import_api_key_provider_with_metadata(
    store: &ConfigStore,
    input: ApiKeyProviderImportRequest,
    provider_id: Option<String>,
    account_email: Option<String>,
) -> Result<ProviderImportOutcome> {
    let ApiKeyProviderImportRequest {
        provider_name,
        kind,
        base_url,
        websocket_url,
        api_key,
        env_var,
        model,
        refresh_interval_seconds,
    } = input;
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

    let (auth_ref, auth_contents) = if let Some(api_key) = api_key {
        if let Some(parent) = auth_path.parent() {
            fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        let auth = serde_json::json!({
            "api_key": api_key,
        });
        let text = serde_json::to_string_pretty(&auth).map_err(|source| {
            CompanionError::InvalidConfig(format!("provider API key serialize failed: {source}"))
        })?;
        (
            Some(format!("file:{}", auth_path.display())),
            Some(format!("{text}\n")),
        )
    } else {
        (env_var.as_ref().map(|value| format!("env:{value}")), None)
    };
    let direct_auth_ref = env_var.as_ref().map(|value| format!("env:{value}"));

    let mut model_map = BTreeMap::new();
    if let Some(model) = model {
        model_map.insert(model.clone(), model);
    }
    let existed = store.load()?.providers.contains_key(&provider_id);
    let provider_input = ProviderUpsert {
        id: provider_id,
        name: provider_name.clone(),
        kind,
        base_url,
        websocket_url,
        auth_ref,
        direct_auth_ref,
        model_map,
        priority: 100,
        enabled: true,
        refresh_interval_seconds: refresh_interval_seconds
            .unwrap_or_else(default_refresh_interval_seconds),
        account: Some(ProviderAccountInfo {
            auth_mode: Some("apikey".to_string()),
            display_name: Some(provider_name.clone()),
            email: account_email,
            subscription_type: Some("API Key".to_string()),
            subscription_status: Some("待检查".to_string()),
            ..ProviderAccountInfo::default()
        }),
    };
    let persist_provider = || add_provider(store, provider_input);
    let provider = match auth_contents {
        Some(auth_contents) => {
            persist_with_private_auth_file(&auth_path, &auth_contents, persist_provider)
        }
        None => persist_provider(),
    }?;

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

fn prepare_api_key_provider_from_json(
    value: &serde_json::Value,
    explicit_provider_id: Option<&str>,
    explicit_provider_name: Option<&str>,
) -> Result<ProviderImportPlan> {
    let api_key = extract_api_key(value).ok_or_else(|| {
        let message = if is_newapi_channel_connection(value) {
            "New API 连接 JSON 缺少 key"
        } else {
            "API Key JSON 缺少 OPENAI_API_KEY"
        };
        CompanionError::InvalidConfig(message.to_string())
    })?;
    if looks_like_http_url(&api_key) {
        return Err(CompanionError::InvalidConfig(
            "API Key 不能是 URL，请检查 JSON 字段是否填反".to_string(),
        ));
    }
    let base_url = if is_newapi_channel_connection(value) {
        extract_api_base_url(value).ok_or_else(|| {
            CompanionError::InvalidConfig("New API 连接 JSON 缺少 url".to_string())
        })?
    } else {
        extract_api_base_url(value).unwrap_or_else(default_openai_api_base_url)
    };
    if !looks_like_http_url(&base_url) {
        return Err(CompanionError::InvalidConfig(format!(
            "API Key JSON 的 api_base_url 无效: {base_url}"
        )));
    }
    let provider_name = explicit_provider_name
        .and_then(normalize_non_empty)
        .or_else(|| pick_string(value, &[&["api_provider_name"], &["apiProviderName"]]))
        .or_else(|| pick_string(value, &[&["provider_name"], &["providerName"], &["name"]]))
        .or_else(|| Some(provider_name_from_base_url(Some(&base_url))))
        .unwrap_or_else(|| "OpenAI API Key".to_string());
    let provider_id = explicit_provider_id
        .and_then(sanitize_provider_id)
        .or_else(|| {
            pick_string(value, &[&["api_provider_id"], &["apiProviderId"]])
                .as_deref()
                .and_then(sanitize_provider_id)
        })
        .unwrap_or_else(|| derive_api_key_provider_id(&provider_name, &base_url));
    let email = pick_string(value, &[&["email"], &["account", "email"]]);
    let model = extract_model(value);
    let kind = infer_api_key_provider_kind(value, &base_url);
    let websocket_url = extract_websocket_url(value);

    Ok(ProviderImportPlan::ApiKey {
        input: ApiKeyProviderImportRequest {
            provider_name,
            kind,
            base_url,
            websocket_url,
            api_key,
            env_var: None,
            model,
            refresh_interval_seconds: None,
        },
        provider_id,
        account_email: email,
    })
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
    if extract_agent_identity_auth(&value).is_some() {
        let plan = prepare_provider_import(
            store,
            &value,
            None,
            config_provider.provider_name.as_deref(),
        )?;
        let mut outcome = execute_provider_import(store, plan)?;
        if let Some(model) = config_provider.model.as_ref() {
            let provider = store.update(|config| {
                let provider = config
                    .providers
                    .get_mut(&outcome.provider.id)
                    .ok_or_else(|| {
                        CompanionError::InvalidConfig(format!(
                            "本地 Agent Identity provider 导入后不存在: {}",
                            outcome.provider.id
                        ))
                    })?;
                provider.model_map.insert(model.clone(), model.clone());
                Ok(provider.clone())
            })?;
            outcome.provider = provider;
        }
        outcome.message = if outcome.created {
            "已导入本机 Agent Identity；该账号仅通过 Companion API 服务动态签名".to_string()
        } else {
            "已更新本机 Agent Identity；该账号仅通过 Companion API 服务动态签名".to_string()
        };
        return Ok(outcome);
    }
    if is_auth_mode_api_key(&value) {
        let api_key = extract_api_key(&value).ok_or_else(|| {
            CompanionError::InvalidConfig("auth.json 缺少 OPENAI_API_KEY".to_string())
        })?;
        return import_api_key_provider_request(
            store,
            ApiKeyProviderImportRequest {
                provider_name: config_provider.provider_name.clone().unwrap_or_else(|| {
                    provider_name_from_base_url(config_provider.base_url.as_deref())
                }),
                kind: ProviderKind::OpenAiCompatible,
                base_url: config_provider
                    .base_url
                    .clone()
                    .unwrap_or_else(default_openai_api_base_url),
                websocket_url: None,
                api_key,
                env_var: config_provider.api_key_env_var.clone(),
                model: config_provider.model.clone(),
                refresh_interval_seconds: None,
            },
        );
    }

    if value.get("tokens").is_some() {
        let live_auth_ref = format!("file:{}", auth_path.display());
        let config = store.load()?;
        let existing_local_provider_id = config
            .groups
            .get(&config.relay.active_group_id)
            .into_iter()
            .flat_map(|group| group.provider_order.iter())
            .filter_map(|id| config.providers.get(id).map(|provider| (id, provider)))
            .chain(config.providers.iter())
            .find(|(_, provider)| {
                provider.kind == ProviderKind::OfficialCodex
                    && (provider.auth_ref.as_deref() == Some(live_auth_ref.as_str())
                        || provider.direct_auth_ref.as_deref() == Some(live_auth_ref.as_str()))
            })
            .map(|(id, _)| id.clone());
        let mut outcome = import_provider_json(store, &text, existing_local_provider_id, None)?;
        let provider = store.update(|config| {
            let provider = config
                .providers
                .get_mut(&outcome.provider.id)
                .ok_or_else(|| {
                    CompanionError::InvalidConfig(format!(
                        "本地 Codex provider 导入后不存在: {}",
                        outcome.provider.id
                    ))
                })?;
            provider.auth_ref = Some(live_auth_ref.clone());
            provider.direct_auth_ref = Some(live_auth_ref);
            if let Some(model) = config_provider.model.as_ref() {
                provider.model_map.insert(model.clone(), model.clone());
            }
            Ok(provider.clone())
        })?;
        outcome.provider = provider;
        outcome.auth_path = auth_path;
        outcome.message = if outcome.created {
            "已导入本地 Codex 账号，并跟随 live auth.json 自动续期".to_string()
        } else {
            "已更新本地 Codex 账号，并切换为 live auth.json 自动续期".to_string()
        };
        return Ok(outcome);
    }

    if let Some(api_key) = extract_api_key(&value) {
        return import_api_key_provider_request(
            store,
            ApiKeyProviderImportRequest {
                provider_name: config_provider.provider_name.clone().unwrap_or_else(|| {
                    provider_name_from_base_url(config_provider.base_url.as_deref())
                }),
                kind: ProviderKind::OpenAiCompatible,
                base_url: config_provider
                    .base_url
                    .clone()
                    .unwrap_or_else(default_openai_api_base_url),
                websocket_url: None,
                api_key,
                env_var: config_provider.api_key_env_var.clone(),
                model: config_provider.model.clone(),
                refresh_interval_seconds: None,
            },
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
            stable_hash(auth.to_string())
                .chars()
                .take(8)
                .collect::<String>()
        )
    });
    let user_id = extract_oauth_user_id(value, &auth);
    let provider_name = explicit_provider_name
        .and_then(normalize_non_empty)
        .or_else(|| extract_oauth_account_name(&auth))
        .unwrap_or_else(|| "Codex 官方账号".to_string());
    let provider_id = explicit_provider_id
        .and_then(sanitize_provider_id)
        .unwrap_or_else(|| {
            derive_oauth_provider_id(
                &provider_name,
                &account_identity_key(&account_id, user_id.as_deref()),
            )
        });
    let model = extract_model(value);

    Ok(ProviderImportDraft {
        provider_id: provider_id.clone(),
        provider_name,
        import_kind: "openai_account".to_string(),
        base_url: OFFICIAL_CODEX_BASE_URL.to_string(),
        auth_ref: format!("file:<companion-data-dir>/auth/accounts/{provider_id}.json"),
        account_id,
        user_id,
        model,
    })
}

fn unsupported_import_error() -> CompanionError {
    CompanionError::InvalidConfig(
        "仅支持 Codex Companion/CPA/sub2api 的 Codex OAuth、Agent Identity 或 API Key 账号 JSON"
            .to_string(),
    )
}

fn extract_agent_identity_auth(value: &serde_json::Value) -> Option<serde_json::Value> {
    let candidate = value
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .and_then(|accounts| accounts.first())
        .unwrap_or(value);
    let credentials = candidate.get("credentials").unwrap_or(candidate);
    let identity = candidate
        .get("agent_identity")
        .or_else(|| candidate.get("agentIdentity"))
        .or_else(|| credentials.get("agent_identity"))
        .or_else(|| credentials.get("agentIdentity"))
        .unwrap_or(credentials);
    let auth_mode = pick_first_string(
        &[identity, credentials, candidate, value],
        &[&["auth_mode"], &["authMode"], &["openai_auth_mode"]],
    );
    let runtime_id = pick_first_string(
        &[identity, credentials, candidate, value],
        &[&["agent_runtime_id"], &["agentRuntimeId"]],
    );
    let private_key = pick_first_string(
        &[identity, credentials, candidate, value],
        &[&["agent_private_key"], &["agentPrivateKey"]],
    );
    let is_agent_identity = auth_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("agentIdentity"))
        || (runtime_id.is_some() && private_key.is_some());
    if !is_agent_identity {
        return None;
    }

    let mut auth = serde_json::Map::new();
    auth.insert(
        "auth_mode".to_string(),
        serde_json::Value::String("agentIdentity".to_string()),
    );
    insert_optional_string(&mut auth, "agent_runtime_id", runtime_id);
    insert_optional_string(&mut auth, "agent_private_key", private_key);
    insert_optional_string(
        &mut auth,
        "task_id",
        pick_first_string(
            &[identity, credentials, candidate, value],
            &[&["task_id"], &["taskId"]],
        ),
    );
    for (target, paths) in [
        (
            "chatgpt_account_id",
            &[
                &["chatgpt_account_id"][..],
                &["account_id"][..],
                &["accountId"][..],
            ][..],
        ),
        (
            "chatgpt_user_id",
            &[&["chatgpt_user_id"][..], &["user_id"][..], &["userId"][..]][..],
        ),
        ("email", &[&["email"][..], &["account", "email"][..]][..]),
        (
            "name",
            &[&["name"][..], &["display_name"][..], &["displayName"][..]][..],
        ),
        (
            "plan_type",
            &[
                &["plan_type"][..],
                &["planType"][..],
                &["chatgpt_plan_type"][..],
            ][..],
        ),
    ] {
        insert_optional_string(
            &mut auth,
            target,
            pick_first_string(&[identity, credentials, candidate, value], paths),
        );
    }
    if let Some(fedramp) = [identity, credentials, candidate, value]
        .iter()
        .find_map(|source| {
            source
                .get("chatgpt_account_is_fedramp")
                .or_else(|| source.get("chatgptAccountIsFedramp"))
                .and_then(serde_json::Value::as_bool)
        })
    {
        auth.insert(
            "chatgpt_account_is_fedramp".to_string(),
            serde_json::Value::Bool(fedramp),
        );
    }
    Some(serde_json::Value::Object(auth))
}

fn validate_agent_identity_auth(auth: &serde_json::Value) -> Result<()> {
    let runtime_id = pick_string(auth, &[&["agent_runtime_id"]]).ok_or_else(|| {
        CompanionError::InvalidConfig("Agent Identity 缺少 agent_runtime_id".to_string())
    })?;
    if runtime_id.trim().is_empty() {
        return Err(CompanionError::InvalidConfig(
            "Agent Identity agent_runtime_id 为空".to_string(),
        ));
    }
    let encoded = pick_string(auth, &[&["agent_private_key"]]).ok_or_else(|| {
        CompanionError::InvalidConfig("Agent Identity 缺少 agent_private_key".to_string())
    })?;
    let der = general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| {
            CompanionError::InvalidConfig("Agent Identity 私钥不是有效 Base64".to_string())
        })?;
    SigningKey::from_pkcs8_der(&der).map_err(|_| {
        CompanionError::InvalidConfig(
            "Agent Identity 私钥不是有效的 PKCS#8 Ed25519 私钥".to_string(),
        )
    })?;
    Ok(())
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
            &["personal_access_token"],
            &["personalAccessToken"],
            &["tokens", "access_token"],
            &["tokens", "accessToken"],
            &["tokens", "personal_access_token"],
            &["tokens", "personalAccessToken"],
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

    let token_claims = [id_token.as_deref(), access_token.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(decode_jwt_payload)
        .collect::<Vec<_>>();
    let mut identity_sources = vec![credentials, extra, candidate, value];
    identity_sources.extend(token_claims.iter());

    let account_id = pick_first_string(
        &identity_sources,
        &[
            &["chatgpt_account_id"],
            &["chatgptAccountId"],
            &["account_id"],
            &["accountId"],
            &["tokens", "chatgpt_account_id"],
            &["tokens", "chatgptAccountId"],
            &["tokens", "account_id"],
            &["workspace_id"],
            &["headers", "ChatGPT-Account-Id"],
            &["custom_headers", "ChatGPT-Account-Id"],
            &["customHeaders", "ChatGPT-Account-Id"],
        ],
    );
    let user_id = pick_first_string(
        &identity_sources,
        &[
            &["chatgpt_user_id"],
            &["user_id"],
            &["userId"],
            &["tokens", "chatgpt_user_id"],
            &["tokens", "user_id"],
        ],
    );
    let email = pick_first_string(&identity_sources, &[&["email"], &["tokens", "email"]]);
    let name = pick_first_string(
        &identity_sources,
        &[
            &["name"],
            &["display_name"],
            &["displayName"],
            &["email"],
            &["tokens", "name"],
            &["tokens", "email"],
        ],
    );
    let plan_type = pick_first_string(
        &identity_sources,
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
    insert_optional_string(&mut tokens, "user_id", user_id.clone());
    insert_optional_string(&mut tokens, "chatgpt_user_id", user_id);
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

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .or_else(|_| general_purpose::URL_SAFE.decode(payload.as_bytes()))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
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
    let api_key = pick_string(
        value,
        &[
            &["OPENAI_API_KEY"],
            &["openai_api_key"],
            &["api_key"],
            &["apiKey"],
            &["credentials", "api_key"],
            &["tokens", "api_key"],
        ],
    );
    api_key.or_else(|| {
        is_newapi_channel_connection(value)
            .then(|| pick_string(value, &[&["key"]]))
            .flatten()
    })
}

fn extract_api_base_url(value: &serde_json::Value) -> Option<String> {
    let base_url = pick_string(
        value,
        &[
            &["api_base_url"],
            &["apiBaseUrl"],
            &["base_url"],
            &["baseUrl"],
            &["credentials", "api_base_url"],
            &["credentials", "base_url"],
        ],
    );
    if let Some(base_url) = base_url {
        return Some(base_url.trim_end_matches('/').to_string());
    }

    is_newapi_channel_connection(value)
        .then(|| pick_string(value, &[&["url"]]))
        .flatten()
        .map(normalize_newapi_channel_base_url)
}

fn is_newapi_channel_connection(value: &serde_json::Value) -> bool {
    value
        .get("_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "newapi_channel_conn")
}

fn normalize_newapi_channel_base_url(base_url: String) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1")
        || base_url.ends_with("/responses")
        || base_url.ends_with("/chat/completions")
    {
        return base_url.to_string();
    }
    format!("{base_url}/v1")
}

fn extract_websocket_url(value: &serde_json::Value) -> Option<String> {
    pick_string(
        value,
        &[
            &["websocket_url"],
            &["websocketUrl"],
            &["ws_url"],
            &["wsUrl"],
            &["credentials", "websocket_url"],
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
    let identity = auth
        .get("agent_identity")
        .or_else(|| auth.get("agentIdentity"))
        .unwrap_or(&serde_json::Value::Null);
    let sources = [identity, credentials, extra, candidate, value, tokens, auth];

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
            &["subscription_expires_at"],
            &["subscriptionExpiresAt"],
            &["subscription_active_until"],
            &["subscriptionActiveUntil"],
            &["chatgpt_subscription_active_until"],
            &["active_until"],
            &["activeUntil"],
            &["entitlement", "subscription_active_until"],
            &["entitlement", "expires_at"],
            &["expires_at"],
            &["expiresAt"],
            &["expired"],
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
        auth_mode: pick_first_string(
            &sources,
            &[&["auth_mode"], &["authMode"], &["openai_auth_mode"]],
        )
        .or_else(|| {
            auth.get("agent_runtime_id")
                .is_some()
                .then(|| "agentIdentity".to_string())
        }),
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

fn extract_oauth_user_id(source: &serde_json::Value, auth: &serde_json::Value) -> Option<String> {
    pick_first_string(
        &[
            auth,
            auth.get("tokens").unwrap_or(&serde_json::Value::Null),
            source,
        ],
        &[
            &["chatgpt_user_id"],
            &["user_id"],
            &["tokens", "chatgpt_user_id"],
            &["tokens", "user_id"],
            &["credentials", "chatgpt_user_id"],
            &["credentials", "user_id"],
        ],
    )
}

fn extract_oauth_account_name(auth: &serde_json::Value) -> Option<String> {
    pick_first_string(
        &[auth, auth.get("tokens").unwrap_or(&serde_json::Value::Null)],
        &[
            &["email"],
            &["name"],
            &["tokens", "email"],
            &["tokens", "name"],
        ],
    )
    .filter(|label| !is_generic_official_account_name(label))
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

fn is_generic_official_account_name(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "codex 官方账号" | "官方账号" | "codex official account" | "official account"
    )
}

fn derive_oauth_provider_id(provider_name: &str, account_id: &str) -> String {
    let name = sanitize_provider_id(provider_name).unwrap_or_else(|| "chatgpt".to_string());
    format!(
        "codex_openai_{}_{}",
        name,
        stable_hash(account_id).chars().take(8).collect::<String>()
    )
}

fn account_identity_key(account_id: &str, user_id: Option<&str>) -> String {
    match user_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(user_id) => format!("{account_id}\0{user_id}"),
        None => account_id.to_string(),
    }
}

fn existing_provider_id_for_identity(
    store: &ConfigStore,
    account_id: &str,
    user_id: Option<&str>,
) -> Result<Option<String>> {
    let user_id = user_id.map(str::trim).filter(|value| !value.is_empty());
    let config = store.load()?;
    Ok(config.providers.values().find_map(|provider| {
        let account = provider.account.as_ref()?;
        if account.account_id.as_deref() != Some(account_id) {
            return None;
        }
        let existing_user_id = account
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        (existing_user_id == user_id).then(|| provider.id.clone())
    }))
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
    use crypto_box::aead::OsRng;
    use ed25519_dalek::pkcs8::EncodePrivateKey;

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
    fn uses_friendly_official_name_when_profile_has_no_label() {
        let value = serde_json::json!({
            "access_token": "access-token",
            "account_id": "account-id",
            "name": "Codex 官方账号"
        });

        let draft = parse_provider_import_draft(&value, None, None).expect("draft");

        assert_eq!(draft.provider_name, "Codex 官方账号");
    }

    #[test]
    fn prefers_official_account_email_over_generic_name() {
        let value = serde_json::json!({
            "access_token": "access-token",
            "account_id": "account-id",
            "email": "person@example.com",
            "name": "Codex 官方账号"
        });

        let draft = parse_provider_import_draft(&value, None, None).expect("draft");

        assert_eq!(draft.provider_name, "person@example.com");
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
    fn imports_cockpit_personal_access_token_with_workspace_header() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "personal_access_token": "at-cockpit-token",
            "email": "team@example.com",
            "customHeaders": {
                "ChatGPT-Account-Id": "workspace-from-header"
            }
        });

        let outcome = import_provider_json(&store, &value.to_string(), None, None)
            .expect("personal access token import");
        let auth: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&outcome.auth_path).expect("auth file"))
                .expect("auth json");

        assert_eq!(outcome.provider.kind, ProviderKind::OfficialCodex);
        assert_eq!(outcome.account_id, "workspace-from-header");
        assert_eq!(
            outcome
                .provider
                .account
                .as_ref()
                .and_then(|account| account.account_id.as_deref()),
            Some("workspace-from-header")
        );
        assert_eq!(auth["tokens"]["access_token"], "at-cockpit-token");
        assert_eq!(
            auth["tokens"]["chatgpt_account_id"],
            "workspace-from-header"
        );
    }

    #[test]
    fn sub2api_subscription_expiry_wins_over_access_token_expiry() {
        let value = serde_json::json!({
            "accounts": [{
                "name": "team@example.com",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "at-team-token",
                    "chatgpt_account_id": "workspace-id",
                    "expires_at": "2026-08-04T00:00:00Z",
                    "subscription_expires_at": "2027-01-02T03:04:05Z"
                }
            }]
        });
        let auth = extract_codex_oauth_auth(&value).expect("oauth auth");

        let account = extract_provider_account_info(&value, &auth);

        assert_eq!(account.valid_until.as_deref(), Some("2027-01-02T03:04:05Z"));
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

        let outcomes = import_provider_json_many(&store, &value.to_string(), None, None, None)
            .expect("import");
        let config = store.load().expect("config");

        assert_eq!(outcomes.succeeded.len(), 2);
        assert_eq!(config.providers.len(), 2);
        assert!(outcomes
            .succeeded
            .iter()
            .any(|outcome| outcome.account_id == "account-a"));
        assert!(outcomes
            .succeeded
            .iter()
            .any(|outcome| outcome.account_id == "account-b"));
    }

    #[test]
    fn batch_import_reports_partial_success_and_joins_the_requested_group() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!([
            {
                "auth_mode": "apikey",
                "OPENAI_API_KEY": "sk-valid",
                "api_base_url": "https://valid.example.com/v1",
                "api_provider_id": "valid_provider",
                "api_provider_name": "Valid Provider"
            },
            {
                "auth_mode": "apikey",
                "api_base_url": "https://invalid.example.com/v1",
                "api_provider_id": "invalid_provider"
            }
        ]);

        let report = import_provider_json_many(
            &store,
            &value.to_string(),
            None,
            None,
            Some(codex_companion_core::DEFAULT_GROUP_ID.to_string()),
        )
        .expect("batch import");
        let config = store.load().expect("config");

        assert_eq!(report.total, 2);
        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].index, 1);
        assert!(report.failed[0].message.contains("API Key"));
        assert_eq!(report.added_to_group, vec!["valid_provider".to_string()]);
        assert_eq!(
            config.groups[codex_companion_core::DEFAULT_GROUP_ID].provider_order,
            vec!["valid_provider".to_string()]
        );
    }

    #[test]
    fn import_review_is_read_only_and_never_returns_credentials() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!([
            {
                "auth_mode": "apikey",
                "OPENAI_API_KEY": "sk-review-secret",
                "api_base_url": "https://review.example.com/v1",
                "api_provider_id": "review_provider",
                "api_provider_name": "Review Provider",
                "model": "review-model"
            },
            {
                "auth_mode": "apikey",
                "api_base_url": "https://invalid.example.com/v1"
            }
        ]);

        let report =
            review_provider_json_many(&store, &value.to_string(), None, None).expect("review");
        let serialized = serde_json::to_string(&report).expect("serialize review");

        assert_eq!(report.total, 2);
        assert_eq!(report.ready.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.ready[0].provider_id, "review_provider");
        assert_eq!(report.ready[0].credential_kind, "API Key");
        assert_eq!(report.ready[0].model.as_deref(), Some("review-model"));
        assert!(!report.ready[0].will_overwrite);
        assert!(!serialized.contains("sk-review-secret"));
        assert!(store.load().expect("config").providers.is_empty());
        assert!(!store.data_dir().join("auth").exists());
    }

    #[test]
    fn failed_provider_persistence_rolls_back_private_auth_file() {
        let temp = tempfile::tempdir().expect("temp");
        let existing_path = temp.path().join("existing-auth.json");
        let new_path = temp.path().join("new-auth.json");
        fs::write(&existing_path, b"old-secret").expect("seed existing auth");

        let existing_result: Result<()> =
            persist_with_private_auth_file(&existing_path, "new-secret", || {
                Err(CompanionError::InvalidConfig("persist failed".to_string()))
            });
        let new_result: Result<()> =
            persist_with_private_auth_file(&new_path, "new-secret", || {
                Err(CompanionError::InvalidConfig("persist failed".to_string()))
            });

        assert!(existing_result.is_err());
        assert!(new_result.is_err());
        assert_eq!(
            fs::read(&existing_path).expect("restored auth"),
            b"old-secret"
        );
        assert!(!new_path.exists());
    }

    #[test]
    fn import_review_resolves_the_same_existing_oauth_provider_as_import() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let original = serde_json::json!({
            "access_token": "access-original",
            "chatgpt_account_id": "shared-account",
            "chatgpt_user_id": "shared-user",
            "email": "person@example.com"
        });
        let imported = import_provider_json(&store, &original.to_string(), None, None)
            .expect("initial import");
        let replacement = serde_json::json!({
            "access_token": "access-replacement",
            "chatgpt_account_id": "shared-account",
            "chatgpt_user_id": "shared-user",
            "email": "renamed@example.com"
        });

        let report = review_provider_json_many(&store, &replacement.to_string(), None, None)
            .expect("review");

        assert_eq!(report.ready.len(), 1);
        assert_eq!(report.ready[0].provider_id, imported.provider.id);
        assert!(report.ready[0].will_overwrite);
        assert!(!serde_json::to_string(&report)
            .expect("serialize review")
            .contains("access-replacement"));
    }

    #[test]
    fn import_review_marks_later_duplicate_targets_as_overwrites() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!([
            {
                "OPENAI_API_KEY": "sk-first-secret",
                "api_provider_id": "duplicate-provider",
                "api_provider_name": "First",
            },
            {
                "OPENAI_API_KEY": "sk-second-secret",
                "api_provider_id": "duplicate-provider",
                "api_provider_name": "Second",
            }
        ]);

        let report = review_provider_json_many(&store, &value.to_string(), None, None)
            .expect("review duplicates");

        assert_eq!(report.ready.len(), 2);
        assert!(!report.ready[0].will_overwrite);
        assert!(report.ready[1].will_overwrite);
        assert_eq!(report.ready[0].provider_id, report.ready[1].provider_id);
    }

    #[test]
    fn same_chatgpt_account_with_different_users_creates_distinct_providers() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let first = serde_json::json!({
            "access_token": "access-a",
            "chatgpt_account_id": "shared-account",
            "chatgpt_user_id": "user-a",
            "email": "first@example.com"
        });
        let second = serde_json::json!({
            "access_token": "access-b",
            "chatgpt_account_id": "shared-account",
            "chatgpt_user_id": "user-b",
            "email": "second@example.com"
        });

        let first =
            import_provider_json(&store, &first.to_string(), None, None).expect("first import");
        let second =
            import_provider_json(&store, &second.to_string(), None, None).expect("second import");
        let config = store.load().expect("config");

        assert_ne!(first.provider.id, second.provider.id);
        assert_eq!(config.providers.len(), 2);
        assert_eq!(
            first
                .provider
                .account
                .as_ref()
                .and_then(|account| account.user_id.as_deref()),
            Some("user-a")
        );
        assert_eq!(
            second
                .provider
                .account
                .as_ref()
                .and_then(|account| account.user_id.as_deref()),
            Some("user-b")
        );
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
        fs::write(codex_dir.join("config.toml"), "model = \"gpt-live\"\n").expect("config");

        let outcome = import_local_codex_provider(&store, Some(codex_dir.clone())).expect("import");
        assert_eq!(outcome.provider.kind, ProviderKind::OfficialCodex);
        assert_eq!(outcome.auth_path, codex_dir.join("auth.json"));
        assert_eq!(
            outcome.provider.auth_ref.as_deref(),
            Some(format!("file:{}", codex_dir.join("auth.json").display()).as_str())
        );
        assert_eq!(outcome.provider.auth_ref, outcome.provider.direct_auth_ref);
        assert_eq!(
            outcome
                .provider
                .model_map
                .get("gpt-live")
                .map(String::as_str),
            Some("gpt-live")
        );
        let provider_id = outcome.provider.id.clone();
        let repeated =
            import_local_codex_provider(&store, Some(codex_dir.clone())).expect("reimport");
        assert_eq!(repeated.provider.id, provider_id);
        assert_eq!(store.load().expect("config").providers.len(), 1);
        assert_eq!(
            outcome
                .provider
                .account
                .and_then(|account| account.account_id),
            Some("local-account".to_string())
        );
    }

    #[test]
    fn imports_local_codex_identity_from_jwt_claims() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("companion").join("config.json"));
        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        let claims = serde_json::json!({
            "email": "person@example.com",
            "name": "Person Example",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-from-jwt",
                "chatgpt_user_id": "user-from-jwt",
                "chatgpt_plan_type": "plus"
            }
        });
        let payload = general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        let id_token = format!("header.{payload}.signature");
        fs::write(
            codex_dir.join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "access_token": "access-token",
                    "id_token": id_token,
                    "refresh_token": "refresh-token"
                }
            })
            .to_string(),
        )
        .expect("auth");

        let outcome = import_local_codex_provider(&store, Some(codex_dir)).expect("import");
        let account = outcome.provider.account.expect("account");

        assert_eq!(outcome.provider.name, "person@example.com");
        assert_eq!(account.email.as_deref(), Some("person@example.com"));
        assert_eq!(account.display_name.as_deref(), Some("Person Example"));
        assert_eq!(account.account_id.as_deref(), Some("account-from-jwt"));
        assert_eq!(account.user_id.as_deref(), Some("user-from-jwt"));
        assert_eq!(account.subscription_type.as_deref(), Some("PLUS"));
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
    fn imports_local_codex_agent_identity_into_companion_storage() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("companion").join("config.json"));
        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key =
            general_purpose::STANDARD.encode(signing_key.to_pkcs8_der().expect("pkcs8").as_bytes());
        fs::write(
            codex_dir.join("auth.json"),
            serde_json::json!({
                "auth_mode": "agentIdentity",
                "agent_runtime_id": "runtime-local",
                "agent_private_key": private_key,
                "task_id": "task-local",
                "chatgpt_account_id": "account-local",
                "chatgpt_user_id": "user-local",
                "email": "agent@example.com"
            })
            .to_string(),
        )
        .expect("auth");
        fs::write(codex_dir.join("config.toml"), "model = \"gpt-agent\"\n").expect("config");

        let outcome = import_local_codex_provider(&store, Some(codex_dir.clone())).expect("import");

        assert_eq!(outcome.import_kind, "agent_identity");
        assert_eq!(outcome.provider.kind, ProviderKind::OfficialCodex);
        assert_ne!(outcome.auth_path, codex_dir.join("auth.json"));
        assert!(outcome
            .auth_path
            .starts_with(store.data_dir().join("auth/accounts")));
        assert_eq!(outcome.provider.direct_auth_ref, None);
        assert_eq!(
            outcome
                .provider
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
        assert!(outcome.provider.model_map.contains_key("gpt-agent"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&outcome.auth_path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
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
            "websocket_url": "wss://sub2api.example.com/v1/responses/",
            "api_provider_id": "sub2api_test",
            "api_provider_name": "Sub2API Test"
        });
        let outcome = import_provider_json(&store, &value.to_string(), None, None).expect("import");
        assert_eq!(outcome.provider.id, "sub2api_test");
        assert_eq!(outcome.provider.name, "Sub2API Test");
        assert_eq!(outcome.provider.kind, ProviderKind::RelayProvider);
        assert_eq!(outcome.provider.base_url, "https://sub2api.example.com/v1");
        assert_eq!(
            outcome.provider.websocket_url.as_deref(),
            Some("wss://sub2api.example.com/v1/responses")
        );
        assert_eq!(
            outcome.provider.account.unwrap().email.as_deref(),
            Some("api-key-1234")
        );
    }

    #[test]
    fn imports_newapi_channel_connection_json() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "_type": "newapi_channel_conn",
            "key": "sk-newapi",
            "url": "https://api.rtoc.cc"
        });

        let outcome = import_provider_json(&store, &value.to_string(), None, None).expect("import");

        assert_eq!(outcome.import_kind, "api_key");
        assert_eq!(outcome.provider.name, "api.rtoc.cc");
        assert_eq!(outcome.provider.kind, ProviderKind::RelayProvider);
        assert_eq!(outcome.provider.base_url, "https://api.rtoc.cc/v1");
        let auth = fs::read_to_string(outcome.auth_path).expect("auth file");
        assert!(auth.contains("sk-newapi"));
    }

    #[test]
    fn ignores_generic_key_and_url_json_without_newapi_type() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "key": "not-an-api-key-field",
            "url": "https://example.com"
        });

        let error = import_provider_json(&store, &value.to_string(), None, None)
            .expect_err("generic JSON must not be imported");

        assert!(error.to_string().contains("仅支持"));
    }

    #[test]
    fn rejects_newapi_channel_connection_without_url() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let value = serde_json::json!({
            "_type": "newapi_channel_conn",
            "key": "sk-newapi"
        });

        let error = import_provider_json(&store, &value.to_string(), None, None)
            .expect_err("connection JSON without url must not be imported");

        assert!(error.to_string().contains("缺少 url"));
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
        let outcomes = import_provider_json_many(&store, &value.to_string(), None, None, None)
            .expect("import");
        assert_eq!(outcomes.succeeded.len(), 2);
        let config = store.load().expect("config");
        assert!(config.providers.contains_key("provider_a"));
        assert!(config.providers.contains_key("provider_b"));
    }
}
