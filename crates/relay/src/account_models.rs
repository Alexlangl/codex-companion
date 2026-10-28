use crate::{
    api_service::ApiServiceStore,
    state::RelayState,
    upstream::{send_upstream, upstream_url, UpstreamRequest},
};
use axum::http::{HeaderMap, Method, Uri};
use bytes::Bytes;
use codex_companion_core::{ProviderConfig, ProviderKind};
use codex_companion_provider::{account_identity_key, chatgpt_plan};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(crate) fn fingerprint(provider: &ProviderConfig) -> Option<String> {
    if chatgpt_plan::is_chatgpt_plan(provider) {
        return chatgpt_plan::catalog_fingerprint(provider);
    }
    (provider.kind == ProviderKind::OfficialCodex).then(|| {
        format!(
            "{:x}",
            Sha256::digest(format!(
                "{}|{}|{:?}|{:?}",
                account_identity_key(provider),
                provider.base_url,
                provider.auth_ref,
                provider.account.as_ref().and_then(|a| a.auth_mode.as_ref())
            ))
        )
    })
}

pub(crate) fn cache(
    store: &ApiServiceStore,
    provider: &ProviderConfig,
    value: &Value,
) -> Result<(), String> {
    let Some(fingerprint) = fingerprint(provider) else {
        return Ok(());
    };
    let plan = chatgpt_plan::is_chatgpt_plan(provider);
    let models = if let Some(models) = value["models"].as_array() {
        models
            .iter()
            .filter(|m| !plan || m["visibility"] == "list")
            .filter_map(|m| m["slug"].as_str().or_else(|| m["id"].as_str()))
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else if !plan {
        value["data"]
            .as_array()
            .ok_or("上游模型目录缺少 models/data 数组")?
            .iter()
            .filter_map(|m| m["id"].as_str())
            .map(str::to_string)
            .collect()
    } else {
        return Err("ChatGPT 套餐模型目录缺少 models 数组".into());
    };
    store
        .cache_account_models(&provider.id, &fingerprint, &models)
        .map_err(|e| e.to_string())
}

pub(crate) fn allowed(store: &ApiServiceStore, provider: &ProviderConfig, model: &str) -> bool {
    let Some(fingerprint) = fingerprint(provider) else {
        return true;
    };
    let model = provider
        .model_map
        .get(model)
        .map(String::as_str)
        .unwrap_or(model);
    if !chatgpt_plan::is_chatgpt_plan(provider) && native_reserve_model(model) {
        return true;
    }
    match store
        .account_model_allowed(&provider.id, &fingerprint, model)
        .ok()
        .flatten()
    {
        Some(allowed) => allowed,
        // A cold native account may discover a new GPT/Codex/Reserve alias on
        // its own endpoint. A shared catalog can never grant another account access.
        None => !chatgpt_plan::is_chatgpt_plan(provider) && cold_native_model(model),
    }
}

fn native_reserve_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model == "reserve" || model.starts_with("reserve-")
}

fn cold_native_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    name.starts_with("gpt-")
        || name.starts_with("codex-")
        || name == "codex"
        || name.starts_with("reserve-")
        || name == "reserve"
}

pub(crate) async fn warm_plan_catalog(
    state: &RelayState,
    provider: &ProviderConfig,
    model: Option<&str>,
) {
    let Some(model) = model.filter(|_| chatgpt_plan::is_chatgpt_plan(provider)) else {
        return;
    };
    let Some(fingerprint) = fingerprint(provider) else {
        return;
    };
    if state
        .api_service
        .account_model_allowed(&provider.id, &fingerprint, model)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    if !chatgpt_plan::plan_binding_valid(&state.store, provider) {
        return;
    }
    let fetch = async {
        let uri = Uri::from_static("/v1/models");
        let headers = HeaderMap::new();
        let upstream = upstream_url(provider, &uri);
        let mut response = send_upstream(
            &state.client,
            &state.api_service,
            UpstreamRequest::new(
                provider,
                &Method::GET,
                &uri,
                &headers,
                Bytes::new(),
                &upstream,
            ),
        )
        .await
        .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let config = state.store.load().ok()?;
        response
            .filter_model_catalog(
                &config.relay.account_protection,
                provider,
                false,
                &state.api_service,
            )
            .await
            .ok()
    };
    let _ = tokio::time::timeout(Duration::from_secs(12), fetch).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_native_admission_is_not_a_global_catalog() {
        for model in ["gpt-6.1-terra", "codex-fixture", "reserve-fixture"] {
            assert!(cold_native_model(model));
        }
        for model in ["claude-fixture", "imagegen", "custom"] {
            assert!(!cold_native_model(model));
        }
    }
    #[test]
    fn catalog_grants_are_account_and_credential_specific() {
        let dir = tempfile::tempdir().unwrap();
        let store = ApiServiceStore::from_config_store(&codex_companion_core::ConfigStore::new(
            dir.path().join("config.json"),
        ));
        store.initialize().unwrap();
        store
            .cache_account_models("first", "credential-a", &["gpt-fixture".into()])
            .unwrap();
        assert_eq!(
            store
                .account_model_allowed("first", "credential-a", "gpt-fixture")
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            store
                .account_model_allowed("first", "credential-a", "gpt-other")
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            store
                .account_model_allowed("second", "credential-a", "gpt-fixture")
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .account_model_allowed("first", "credential-b", "gpt-fixture")
                .unwrap(),
            None
        );
    }
}
