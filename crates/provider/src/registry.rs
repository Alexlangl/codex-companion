use crate::types::{ApiKeyProviderUpdate, ProviderUpsert, ProviderUsageQueryUpdate};
use crate::validate::{validate_base_url, validate_id};
use crate::{persist_with_private_auth_file, persist_with_private_auth_file_removal};
use codex_companion_core::{
    CompanionError, ConfigStore, ProviderAccountInfo, ProviderConfig, ProviderGroup,
    ProviderHealth, ProviderKind, ProviderUsageQuery, Result, DEFAULT_GROUP_ID,
};
use std::fs;

pub fn add_provider(store: &ConfigStore, input: ProviderUpsert) -> Result<ProviderConfig> {
    validate_id(&input.id)?;
    validate_base_url(&input.base_url)?;
    store.update(|config| {
        let provider = ProviderConfig {
            id: input.id.trim().to_string(),
            name: input.name.trim().to_string(),
            kind: input.kind,
            base_url: input.base_url.trim().trim_end_matches('/').to_string(),
            websocket_url: input
                .websocket_url
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().trim_end_matches('/').to_string()),
            auth_ref: input.auth_ref.filter(|value| !value.trim().is_empty()),
            direct_auth_ref: input
                .direct_auth_ref
                .filter(|value| !value.trim().is_empty()),
            model_map: input.model_map,
            priority: input.priority,
            enabled: input.enabled,
            refresh_interval_seconds: input.refresh_interval_seconds,
            account: input.account,
        };
        config
            .health
            .entry(provider.id.clone())
            .or_insert_with(ProviderHealth::default);
        config
            .providers
            .insert(provider.id.clone(), provider.clone());
        config
            .groups
            .entry(DEFAULT_GROUP_ID.to_string())
            .or_insert_with(ProviderGroup::default_group);
        Ok(provider)
    })
}

pub fn update_api_key_provider(
    store: &ConfigStore,
    input: ApiKeyProviderUpdate,
) -> Result<ProviderConfig> {
    validate_id(&input.id)?;
    validate_base_url(&input.base_url)?;
    if matches!(input.kind, ProviderKind::OfficialCodex) {
        return Err(CompanionError::InvalidConfig(
            "官方 Codex 账号不能按 API Key provider 编辑".to_string(),
        ));
    }
    let provider_name = normalize_non_empty(&input.provider_name)
        .ok_or_else(|| CompanionError::InvalidConfig("供应商名称不能为空".to_string()))?;
    let provider_display_name = input
        .provider_display_name
        .as_deref()
        .and_then(normalize_non_empty);
    let base_url = input.base_url.trim().trim_end_matches('/').to_string();
    let new_api_key = input.api_key.as_deref().and_then(normalize_non_empty);
    let new_env_var = input.env_var.as_deref().and_then(normalize_non_empty);
    let api_key_updated = new_api_key.is_some();
    let usage_credentials_updated = input.usage_query.as_ref().is_some_and(|query| {
        query.enabled
            && [
                query.api_key.as_deref(),
                query.access_token.as_deref(),
                query.user_id.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| normalize_non_empty(value).is_some())
    });

    let existing = store
        .load()?
        .providers
        .get(&input.id)
        .cloned()
        .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown provider: {}", input.id)))?;
    if matches!(existing.kind, ProviderKind::OfficialCodex) {
        return Err(CompanionError::InvalidConfig(
            "官方 Codex 账号不能按 API Key provider 编辑".to_string(),
        ));
    }

    let mut auth_ref = existing.auth_ref.clone();
    let mut direct_auth_ref = existing.direct_auth_ref.clone();
    let api_key_credential_path = managed_api_key_credential_path(store, &input.id);
    let account_credential_path = managed_account_credential_path(store, &input.id);
    let mut api_key_credential_change = PrivateCredentialChange::None;
    if let Some(api_key) = new_api_key {
        if let Some(parent) = api_key_credential_path.parent() {
            fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        let auth = serde_json::json!({
            "auth_mode": "apikey",
            "OPENAI_API_KEY": api_key,
            "email": provider_display_name.clone().unwrap_or_else(|| provider_name.clone()),
            "api_base_url": base_url.clone(),
            "api_provider_id": input.id.clone(),
            "api_provider_name": provider_name.clone(),
        });
        let text = serde_json::to_string_pretty(&auth).map_err(|source| {
            CompanionError::InvalidConfig(format!("provider API key serialize failed: {source}"))
        })?;
        auth_ref = Some(format!("file:{}", api_key_credential_path.display()));
        direct_auth_ref = None;
        api_key_credential_change =
            PrivateCredentialChange::Write(api_key_credential_path, format!("{text}\n"));
    } else if let Some(env_var) = new_env_var {
        auth_ref = Some(format!("env:{env_var}"));
        direct_auth_ref = Some(format!("env:{env_var}"));
        api_key_credential_change = PrivateCredentialChange::Remove(api_key_credential_path);
    }

    if auth_ref.as_deref().and_then(normalize_non_empty).is_none() {
        return Err(CompanionError::InvalidConfig(
            "API Key provider 缺少 API Key 文件或环境变量".to_string(),
        ));
    }
    let usage_source_changed = api_key_updated
        || auth_ref != existing.auth_ref
        || base_url != existing.base_url
        || input.kind != existing.kind
        || usage_credentials_updated;

    let mut account = existing
        .account
        .clone()
        .unwrap_or_else(ProviderAccountInfo::default);
    if let Some(provider_display_name) = provider_display_name {
        account.email = Some(provider_display_name);
    }
    account.display_name = Some(provider_name.clone());
    account.subscription_type = Some("API Key".to_string());

    let existing_usage_query = existing
        .account
        .as_ref()
        .and_then(|account| account.usage_query.as_ref());
    let (usage_query, usage_query_credential_change) = prepare_usage_query_update(
        store,
        &input.id,
        &base_url,
        existing_usage_query,
        input.usage_query.as_ref(),
    )?;
    if existing_usage_query != usage_query.as_ref() || usage_source_changed {
        clear_usage_snapshot(&mut account);
    }
    account.usage_query = usage_query;

    let provider_input = ProviderUpsert {
        id: input.id,
        name: provider_name,
        kind: input.kind,
        base_url,
        websocket_url: input.websocket_url,
        auth_ref,
        direct_auth_ref,
        model_map: existing.model_map,
        priority: existing.priority,
        enabled: existing.enabled,
        refresh_interval_seconds: input.refresh_interval_seconds,
        account: Some(account),
    };
    let persist_provider = || add_provider(store, provider_input);
    persist_private_credential_change(usage_query_credential_change, || {
        persist_private_credential_change(api_key_credential_change, || {
            persist_private_credential_change(
                PrivateCredentialChange::Remove(account_credential_path),
                persist_provider,
            )
        })
    })
}

fn clear_usage_snapshot(account: &mut ProviderAccountInfo) {
    account.quota_label = None;
    account.quota_percent = None;
    account.quota_reset_at = None;
    account.quota_windows.clear();
    account.usage_total = None;
    account.usage_used = None;
    account.usage_available = None;
    account.last_refresh_at = None;
}

pub(crate) fn prepare_usage_query_update(
    store: &ConfigStore,
    provider_id: &str,
    provider_base_url: &str,
    existing: Option<&ProviderUsageQuery>,
    update: Option<&ProviderUsageQueryUpdate>,
) -> Result<(Option<ProviderUsageQuery>, PrivateCredentialChange)> {
    let credential_path = usage_query_credential_path(store, provider_id);
    let Some(update) = update else {
        return Ok(match existing {
            Some(existing) => (Some(existing.clone()), PrivateCredentialChange::None),
            None => (None, PrivateCredentialChange::Remove(credential_path)),
        });
    };
    if !update.enabled {
        return Ok((None, PrivateCredentialChange::Remove(credential_path)));
    }

    let base_url = update
        .base_url
        .as_deref()
        .and_then(normalize_non_empty)
        .unwrap_or_else(|| provider_base_url.to_string());
    validate_base_url(&base_url)?;
    let existing_credentials = read_usage_query_credentials(&credential_path);
    let api_key = update
        .api_key
        .as_deref()
        .and_then(normalize_non_empty)
        .or_else(|| {
            existing_credentials
                .as_ref()
                .and_then(|value| json_string(value, "api_key"))
        });
    let access_token = update
        .access_token
        .as_deref()
        .and_then(normalize_non_empty)
        .or_else(|| {
            existing_credentials
                .as_ref()
                .and_then(|value| json_string(value, "access_token"))
        });
    let user_id = update
        .user_id
        .as_deref()
        .and_then(normalize_non_empty)
        .or_else(|| {
            existing_credentials
                .as_ref()
                .and_then(|value| json_string(value, "user_id"))
        });
    if matches!(
        update.template,
        codex_companion_core::ProviderUsageQueryTemplate::NewApi
    ) && (access_token.is_none() || user_id.is_none())
    {
        return Err(CompanionError::InvalidConfig(
            "NewAPI 余量查询缺少访问令牌或用户 ID".to_string(),
        ));
    }
    let script = update
        .script
        .as_deref()
        .and_then(normalize_non_empty)
        .unwrap_or_else(|| crate::account_refresh::usage_query_preset(update.template));
    let credentials = serde_json::to_string_pretty(&serde_json::json!({
        "api_key": api_key,
        "access_token": access_token,
        "user_id": user_id,
    }))
    .map_err(|source| CompanionError::InvalidConfig(format!("余量查询凭据序列化失败: {source}")))?;
    Ok((
        Some(ProviderUsageQuery {
            template: update.template,
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            script,
            timeout_seconds: update.timeout_seconds.clamp(2, 30),
        }),
        PrivateCredentialChange::Write(credential_path, format!("{credentials}\n")),
    ))
}

pub(crate) enum PrivateCredentialChange {
    None,
    Write(std::path::PathBuf, String),
    Remove(std::path::PathBuf),
}

pub(crate) fn persist_private_credential_change<T>(
    change: PrivateCredentialChange,
    persist: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match change {
        PrivateCredentialChange::None => persist(),
        PrivateCredentialChange::Write(path, contents) => {
            persist_with_private_auth_file(&path, &contents, persist)
        }
        PrivateCredentialChange::Remove(path) => {
            persist_with_private_auth_file_removal(&path, persist)
        }
    }
}

pub(crate) fn usage_query_credential_path(
    store: &ConfigStore,
    provider_id: &str,
) -> std::path::PathBuf {
    store
        .data_dir()
        .join("auth")
        .join("usage-queries")
        .join(format!("{provider_id}.json"))
}

fn read_usage_query_credentials(path: &std::path::Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(normalize_non_empty)
}

pub fn remove_provider(store: &ConfigStore, id: &str) -> Result<bool> {
    validate_id(id)?;
    if !store.load()?.providers.contains_key(id) {
        return Ok(false);
    }
    let [usage_credential_path, api_key_path, account_path] =
        managed_provider_credential_paths(store, id);
    persist_with_private_auth_file_removal(&usage_credential_path, || {
        persist_with_private_auth_file_removal(&api_key_path, || {
            persist_with_private_auth_file_removal(&account_path, || {
                store.update(|config| {
                    let removed = config.providers.remove(id).is_some();
                    config.health.remove(id);
                    for group in config.groups.values_mut() {
                        group.provider_order.retain(|provider_id| provider_id != id);
                        group.provider_weights.remove(id);
                        if group.priority_failback_target_provider_id.as_deref() == Some(id) {
                            group.priority_failback_target_provider_id = None;
                        }
                    }
                    Ok(removed)
                })
            })
        })
    })
}

fn managed_provider_credential_paths(store: &ConfigStore, id: &str) -> [std::path::PathBuf; 3] {
    [
        usage_query_credential_path(store, id),
        managed_api_key_credential_path(store, id),
        managed_account_credential_path(store, id),
    ]
}

pub(crate) fn managed_api_key_credential_path(store: &ConfigStore, id: &str) -> std::path::PathBuf {
    store
        .data_dir()
        .join("auth")
        .join("api-keys")
        .join(format!("{id}.json"))
}

pub(crate) fn managed_account_credential_path(store: &ConfigStore, id: &str) -> std::path::PathBuf {
    store
        .data_dir()
        .join("auth")
        .join("accounts")
        .join(format!("{id}.json"))
}

pub fn list_providers(store: &ConfigStore) -> Result<Vec<ProviderConfig>> {
    let config = store.load()?;
    Ok(config.providers.into_values().collect())
}

fn normalize_non_empty(value: &str) -> Option<String> {
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
    use crate::types::{ApiKeyProviderUpdate, ProviderUsageQueryUpdate};
    use codex_companion_core::{
        default_refresh_interval_seconds, GroupPolicy, ProviderKind, ProviderUsageQueryTemplate,
    };
    use std::collections::BTreeMap;

    fn provider(id: &str) -> ProviderUpsert {
        ProviderUpsert {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn add_provider_does_not_auto_join_default_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));

        add_provider(&store, provider("a")).expect("add");

        let config = store.load().expect("load");
        assert!(config.groups[DEFAULT_GROUP_ID].provider_order.is_empty());
    }

    #[test]
    fn remove_provider_prunes_group_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        add_provider(&store, provider("a")).expect("add a");
        add_provider(&store, provider("b")).expect("add b");
        store
            .update(|config| {
                config.groups.insert(
                    "work".to_string(),
                    ProviderGroup {
                        id: "work".to_string(),
                        name: "Work".to_string(),
                        policy: GroupPolicy::PriorityFallback,
                        provider_order: vec!["a".to_string(), "b".to_string()],
                        provider_weights: Default::default(),
                        fallback_enabled: true,
                        priority_failback_interval_seconds: 0,
                        priority_failback_revision: 0,
                        priority_failback_target_provider_id: Some("a".to_string()),
                    },
                );
                Ok(())
            })
            .expect("group");

        assert!(remove_provider(&store, "a").expect("remove"));
        let config = store.load().expect("load");
        assert_eq!(config.groups["work"].provider_order, vec!["b".to_string()]);
        assert_eq!(
            config.groups["work"].priority_failback_target_provider_id,
            None
        );
        assert!(config.groups[DEFAULT_GROUP_ID].provider_order.is_empty());
    }

    #[test]
    fn update_api_key_provider_writes_codex_companion_auth_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        add_provider(&store, provider("api-key-provider")).expect("add");

        let updated = update_api_key_provider(
            &store,
            ApiKeyProviderUpdate {
                id: "api-key-provider".to_string(),
                provider_display_name: Some("api-key-demo".to_string()),
                provider_name: "PPTOKEN".to_string(),
                kind: ProviderKind::RelayProvider,
                base_url: "https://cn.pptoken.cc/v1".to_string(),
                websocket_url: None,
                api_key: Some("sk-secret".to_string()),
                env_var: None,
                refresh_interval_seconds: 30,
                usage_query: None,
            },
        )
        .expect("update");

        assert_eq!(updated.name, "PPTOKEN");
        assert_eq!(updated.kind, ProviderKind::RelayProvider);
        let auth_ref = updated.auth_ref.as_deref().expect("auth ref");
        let path = auth_ref.strip_prefix("file:").expect("file ref");
        let auth = std::fs::read_to_string(path).expect("auth file");
        let value = serde_json::from_str::<serde_json::Value>(&auth).expect("json");
        assert_eq!(value["auth_mode"], "apikey");
        assert_eq!(value["OPENAI_API_KEY"], "sk-secret");
        assert_eq!(value["email"], "api-key-demo");
        assert_eq!(value["api_base_url"], "https://cn.pptoken.cc/v1");
        assert_eq!(value["api_provider_name"], "PPTOKEN");
        assert_eq!(
            updated
                .account
                .as_ref()
                .and_then(|account| account.email.as_deref()),
            Some("api-key-demo")
        );
        store
            .update(|config| {
                let account = config
                    .providers
                    .get_mut("api-key-provider")
                    .expect("provider")
                    .account
                    .as_mut()
                    .expect("account");
                account.usage_available = Some(12.5);
                account.last_refresh_at = Some("2026-08-06T08:00:00Z".to_string());
                Ok(())
            })
            .expect("seed usage snapshot");

        let switched = update_api_key_provider(
            &store,
            ApiKeyProviderUpdate {
                id: "api-key-provider".to_string(),
                provider_display_name: Some("api-key-demo".to_string()),
                provider_name: "PPTOKEN".to_string(),
                kind: ProviderKind::RelayProvider,
                base_url: "https://cn.pptoken.cc/v1".to_string(),
                websocket_url: None,
                api_key: None,
                env_var: Some("PPTOKEN_API_KEY".to_string()),
                refresh_interval_seconds: 30,
                usage_query: None,
            },
        )
        .expect("switch to environment variable");
        assert_eq!(switched.auth_ref.as_deref(), Some("env:PPTOKEN_API_KEY"));
        assert_eq!(
            switched.direct_auth_ref.as_deref(),
            Some("env:PPTOKEN_API_KEY")
        );
        assert_eq!(
            switched
                .account
                .as_ref()
                .and_then(|account| account.usage_available),
            None
        );
        assert_eq!(
            switched
                .account
                .as_ref()
                .and_then(|account| account.last_refresh_at.as_deref()),
            None
        );
        assert!(!std::path::Path::new(path).exists());
    }

    #[test]
    fn update_provider_persists_new_api_query_credentials_privately() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        add_provider(&store, provider("configured-usage")).expect("add");

        let updated = update_api_key_provider(
            &store,
            ApiKeyProviderUpdate {
                id: "configured-usage".to_string(),
                provider_display_name: None,
                provider_name: "Configured Usage".to_string(),
                kind: ProviderKind::RelayProvider,
                base_url: "https://api.example.com/v1/responses".to_string(),
                websocket_url: None,
                api_key: Some("sk-test".to_string()),
                env_var: None,
                refresh_interval_seconds: 60,
                usage_query: Some(ProviderUsageQueryUpdate {
                    enabled: true,
                    template: ProviderUsageQueryTemplate::NewApi,
                    base_url: Some("https://balance.example.com".to_string()),
                    script: None,
                    timeout_seconds: 10,
                    api_key: None,
                    access_token: Some("access-test".to_string()),
                    user_id: Some("user-test".to_string()),
                }),
            },
        )
        .expect("configure query");

        let query = updated
            .account
            .as_ref()
            .and_then(|account| account.usage_query.as_ref())
            .expect("query metadata");
        assert_eq!(query.template, ProviderUsageQueryTemplate::NewApi);
        assert_eq!(query.base_url, "https://balance.example.com");
        let credential_path = temp
            .path()
            .join("auth")
            .join("usage-queries")
            .join("configured-usage.json");
        let credentials = fs::read_to_string(&credential_path).expect("credentials");
        let credentials = serde_json::from_str::<serde_json::Value>(&credentials).expect("json");
        assert_eq!(credentials["access_token"], "access-test");
        assert_eq!(credentials["user_id"], "user-test");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&credential_path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        store
            .update(|config| {
                let account = config
                    .providers
                    .get_mut("configured-usage")
                    .expect("provider")
                    .account
                    .as_mut()
                    .expect("account");
                account.quota_label = Some("账户余额".to_string());
                account.usage_available = Some(12.0);
                Ok(())
            })
            .expect("seed snapshot");
        let retained = update_api_key_provider(
            &store,
            ApiKeyProviderUpdate {
                id: "configured-usage".to_string(),
                provider_display_name: None,
                provider_name: "Configured Usage".to_string(),
                kind: ProviderKind::RelayProvider,
                base_url: "https://api.example.com/v1/responses".to_string(),
                websocket_url: None,
                api_key: None,
                env_var: None,
                refresh_interval_seconds: 60,
                usage_query: Some(ProviderUsageQueryUpdate {
                    enabled: true,
                    template: ProviderUsageQueryTemplate::NewApi,
                    base_url: Some("https://balance.example.com".to_string()),
                    script: None,
                    timeout_seconds: 10,
                    api_key: None,
                    access_token: None,
                    user_id: None,
                }),
            },
        )
        .expect("retain credentials");
        assert_eq!(
            retained
                .account
                .as_ref()
                .and_then(|account| account.usage_available),
            Some(12.0)
        );

        let disabled = update_api_key_provider(
            &store,
            ApiKeyProviderUpdate {
                id: "configured-usage".to_string(),
                provider_display_name: None,
                provider_name: "Configured Usage".to_string(),
                kind: ProviderKind::RelayProvider,
                base_url: "https://api.example.com/v1/responses".to_string(),
                websocket_url: None,
                api_key: None,
                env_var: None,
                refresh_interval_seconds: 60,
                usage_query: Some(ProviderUsageQueryUpdate {
                    enabled: false,
                    template: ProviderUsageQueryTemplate::NewApi,
                    base_url: None,
                    script: None,
                    timeout_seconds: 10,
                    api_key: None,
                    access_token: None,
                    user_id: None,
                }),
            },
        )
        .expect("disable query");
        let disabled_account = disabled.account.as_ref().expect("account");
        assert!(disabled_account.usage_query.is_none());
        assert_eq!(disabled_account.quota_label, None);
        assert_eq!(disabled_account.usage_available, None);
        assert_eq!(disabled_account.last_refresh_at, None);
        assert!(!credential_path.exists());
    }

    #[test]
    fn remove_provider_deletes_managed_credentials_but_preserves_external_auth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let external_auth_path = temp.path().join("external-auth.json");
        fs::write(&external_auth_path, r#"{"api_key":"external-secret"}"#).expect("external auth");
        let mut configured = provider("configured-usage");
        configured.auth_ref = Some(format!("file:{}", external_auth_path.display()));
        add_provider(&store, configured).expect("add");
        let credential_paths = managed_provider_credential_paths(&store, "configured-usage");
        for credential_path in &credential_paths {
            fs::create_dir_all(credential_path.parent().expect("parent")).expect("directory");
            fs::write(credential_path, r#"{"secret":"managed"}"#).expect("credentials");
        }

        assert!(remove_provider(&store, "configured-usage").expect("remove"));
        assert!(credential_paths.iter().all(|path| !path.exists()));
        assert!(external_auth_path.exists());
    }

    #[test]
    fn remove_unknown_provider_preserves_orphaned_credentials() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let credential_path = usage_query_credential_path(&store, "unknown");
        fs::create_dir_all(credential_path.parent().expect("parent")).expect("directory");
        fs::write(&credential_path, r#"{"access_token":"secret"}"#).expect("credentials");

        assert!(!remove_provider(&store, "unknown").expect("remove"));
        assert!(credential_path.exists());
        assert!(remove_provider(&store, "../outside").is_err());
    }
}
