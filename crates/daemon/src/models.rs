use crate::runtime::CompanionDaemon;
use chrono::{DateTime, Utc};
use codex_companion_core::{
    account_policy_block_reason, default_codex_dir, http_client_builder, provider_api_base_url,
    redact_sensitive_text, AccountProtection, ModelMatrixModel, ModelMatrixSnapshot,
    ModelMatrixSource, ModelSourceKind, ModelSourceStatus, ProviderConfig, ProviderKind, Result,
};
use codex_companion_provider::{
    ensure_codex_auth_snapshot, provider_uses_agent_identity, provider_uses_codex_oauth,
    resolve_auth_token,
};
use reqwest::{header, Client, Response, StatusCode, Url};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::task::JoinSet;

const MODEL_CACHE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const MODEL_RESPONSE_MAX_BYTES: u64 = 8 * 1024 * 1024;
const MODEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const MODEL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PICKER_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn discover_relay_model_slugs(
    providers: &[ProviderConfig],
    policy: &AccountProtection,
) -> Vec<String> {
    let providers = providers
        .iter()
        .filter(|provider| provider.enabled && provider.kind != ProviderKind::OfficialCodex)
        .filter(|provider| provider.model_map.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if providers.is_empty() {
        return Vec::new();
    }
    let policy = policy.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return Vec::new();
        };
        runtime.block_on(async move {
            let Ok(client) = http_client_builder()
                .timeout(PICKER_DISCOVERY_TIMEOUT)
                .connect_timeout(PICKER_DISCOVERY_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
            else {
                return Vec::new();
            };
            let mut tasks = JoinSet::new();
            for provider in providers {
                let client = client.clone();
                tasks.spawn(async move {
                    let models = fetch_provider_model_response(&client, &provider, true)
                        .await
                        .and_then(|value| parse_model_response(&value, false))
                        .unwrap_or_default();
                    (provider, models)
                });
            }
            let mut slugs = BTreeSet::new();
            while let Some(Ok((provider, models))) = tasks.join_next().await {
                for model in models {
                    if model.visibility.as_deref() != Some("hide")
                        && account_policy_block_reason(
                            &policy,
                            &provider,
                            Some(&model.id),
                            Utc::now(),
                        ) != Some("account_model_excluded")
                    {
                        slugs.insert(model.id);
                    }
                }
            }
            slugs.into_iter().collect()
        })
    })
    .join()
    .unwrap_or_default()
}

#[derive(Debug, Clone)]
struct DiscoveredModel {
    id: String,
    display_name: String,
    reasoning_efforts: Vec<String>,
    multi_agent_version: Option<String>,
    visibility: Option<String>,
    context_window: Option<i64>,
    max_context_window: Option<i64>,
    auto_compact_token_limit: Option<i64>,
    priority: Option<i64>,
    trusted_capabilities: bool,
}

#[derive(Debug)]
struct SourceDiscovery {
    source: ModelMatrixSource,
    models: Vec<DiscoveredModel>,
}

#[derive(Debug)]
struct ModelAccumulator {
    display_name: String,
    source_ids: BTreeSet<String>,
    reasoning_efforts: BTreeSet<String>,
    multi_agent_version: Option<String>,
    ultra_capable: bool,
    visibility: Option<String>,
    context_window: Option<i64>,
    max_context_window: Option<i64>,
    auto_compact_token_limit: Option<i64>,
    priority: Option<i64>,
    trusted_name: bool,
}

impl CompanionDaemon {
    pub async fn model_matrix(&self) -> Result<ModelMatrixSnapshot> {
        let config = self.store.load()?;
        let active_provider_ids = config
            .groups
            .get(&config.relay.active_group_id)
            .map(|group| group.provider_order.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_default();
        let providers = ordered_providers(&config.providers, &active_provider_ids);
        let client = http_client_builder()
            .timeout(MODEL_REQUEST_TIMEOUT)
            .connect_timeout(MODEL_CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| {
                codex_companion_core::CompanionError::InvalidConfig(format!(
                    "创建模型查询客户端失败: {source}"
                ))
            })?;

        let mut discoveries = vec![discover_local_cache(&default_codex_dir()?)];
        let mut tasks = JoinSet::new();
        for (index, provider) in providers.into_iter().enumerate() {
            let client = client.clone();
            let active_group = active_provider_ids.contains(&provider.id);
            tasks.spawn(async move {
                (
                    index,
                    discover_provider_models(&client, provider, active_group).await,
                )
            });
        }

        let mut provider_discoveries = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(discovery) = result {
                provider_discoveries.push(discovery);
            }
        }
        provider_discoveries.sort_by_key(|(index, _)| *index);
        discoveries.extend(
            provider_discoveries
                .into_iter()
                .map(|(_, discovery)| discovery),
        );

        Ok(build_matrix(discoveries))
    }
}

fn ordered_providers(
    providers: &BTreeMap<String, ProviderConfig>,
    active_provider_ids: &HashSet<String>,
) -> Vec<ProviderConfig> {
    let mut providers = providers.values().cloned().collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        provider_sort_key(left, active_provider_ids)
            .cmp(&provider_sort_key(right, active_provider_ids))
            .then_with(|| right.priority.cmp(&left.priority))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    providers
}

fn provider_sort_key(provider: &ProviderConfig, active_provider_ids: &HashSet<String>) -> u8 {
    if provider.kind == ProviderKind::OfficialCodex {
        0
    } else if active_provider_ids.contains(&provider.id) {
        1
    } else {
        2
    }
}

fn discover_local_cache(codex_dir: &Path) -> SourceDiscovery {
    let path = codex_dir.join("models_cache.json");
    let mut source = ModelMatrixSource {
        id: "local-cache".to_string(),
        name: "本地官方缓存".to_string(),
        kind: ModelSourceKind::LocalCache,
        provider_id: None,
        active_group: false,
        status: ModelSourceStatus::Skipped,
        model_count: 0,
        fetched_at: None,
        error: None,
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SourceDiscovery {
                source,
                models: Vec::new(),
            };
        }
        Err(error) => {
            source.status = ModelSourceStatus::Failed;
            source.error = Some(format!("读取模型缓存失败: {error}"));
            return SourceDiscovery {
                source,
                models: Vec::new(),
            };
        }
    };
    if metadata.len() > MODEL_CACHE_MAX_BYTES {
        source.status = ModelSourceStatus::Failed;
        source.error = Some("模型缓存超过 32 MB 限制".to_string());
        return SourceDiscovery {
            source,
            models: Vec::new(),
        };
    }
    let value = match fs::read(&path)
        .map_err(|error| format!("读取模型缓存失败: {error}"))
        .and_then(|bytes| {
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|error| format!("解析模型缓存失败: {error}"))
        }) {
        Ok(value) => value,
        Err(error) => {
            source.status = ModelSourceStatus::Failed;
            source.error = Some(error);
            return SourceDiscovery {
                source,
                models: Vec::new(),
            };
        }
    };
    source.fetched_at = value
        .get("fetched_at")
        .and_then(Value::as_str)
        .and_then(parse_timestamp);
    match parse_model_response(&value, true) {
        Ok(models) => {
            source.status = ModelSourceStatus::Available;
            source.model_count = models.len();
            SourceDiscovery { source, models }
        }
        Err(error) => {
            source.status = ModelSourceStatus::Failed;
            source.error = Some(error);
            SourceDiscovery {
                source,
                models: Vec::new(),
            }
        }
    }
}

async fn discover_provider_models(
    client: &Client,
    provider: ProviderConfig,
    active_group: bool,
) -> SourceDiscovery {
    let kind = if provider.kind == ProviderKind::OfficialCodex {
        if provider_uses_codex_oauth(&provider) {
            ModelSourceKind::OfficialOauth
        } else if provider_uses_agent_identity(&provider) {
            ModelSourceKind::Relay
        } else {
            ModelSourceKind::OfficialPat
        }
    } else {
        ModelSourceKind::Relay
    };
    let mut source = ModelMatrixSource {
        id: format!("provider:{}", provider.id),
        name: provider.name.clone(),
        kind,
        provider_id: Some(provider.id.clone()),
        active_group,
        status: ModelSourceStatus::Skipped,
        model_count: 0,
        fetched_at: None,
        error: None,
    };
    if !provider.enabled {
        return SourceDiscovery {
            source,
            models: Vec::new(),
        };
    }

    let result = fetch_provider_model_response(client, &provider, false).await;
    match result.and_then(|value| parse_model_response(&value, kind != ModelSourceKind::Relay)) {
        Ok(models) => {
            source.status = ModelSourceStatus::Available;
            source.model_count = models.len();
            source.fetched_at = Some(Utc::now());
            SourceDiscovery { source, models }
        }
        Err(error) => {
            source.status = ModelSourceStatus::Failed;
            source.error = Some(error);
            SourceDiscovery {
                source,
                models: Vec::new(),
            }
        }
    }
}

async fn fetch_provider_model_response(
    client: &Client,
    provider: &ProviderConfig,
    codex_catalog: bool,
) -> std::result::Result<Value, String> {
    if provider.kind == ProviderKind::OfficialCodex {
        if provider_uses_agent_identity(provider) {
            return Err("该官方账号不是 OAuth 认证，未查询模型目录".to_string());
        }
        let auth = if provider_uses_codex_oauth(provider) {
            ensure_codex_auth_snapshot(provider)
                .await
                .map_err(|error| format!("OAuth 认证不可用: {error}"))?
        } else {
            let access_token = resolve_auth_token(provider)
                .ok_or_else(|| "官方 PAT 缺少 access_token".to_string())?;
            codex_companion_provider::CodexAuthSnapshot {
                access_token,
                account_id: None,
                email: None,
                name: None,
                plan_type: None,
            }
        };
        let url = format!(
            "{}/models?client_version={}",
            provider.base_url.trim_end_matches('/'),
            env!("CARGO_PKG_VERSION")
        );
        let account_id = auth.account_id.or_else(|| {
            provider
                .account
                .as_ref()
                .and_then(|account| account.account_id.as_deref())
                .filter(|account_id| !account_id.trim().is_empty())
                .map(str::to_string)
        });
        let mut request = client
            .get(&url)
            .bearer_auth(auth.access_token)
            .header("originator", "codex_cli_rs")
            .header("version", "0.144.1");
        if let Some(account_id) = account_id {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        let response = send_model_request(request, &url).await?;
        return parse_provider_response(response).await;
    }

    let mut urls = relay_model_url_candidates(&provider.base_url)?;
    if codex_catalog {
        for url in &mut urls {
            let mut parsed = Url::parse(url).map_err(|_| "Provider Base URL 无效".to_string())?;
            parsed
                .query_pairs_mut()
                .append_pair("client_version", env!("CARGO_PKG_VERSION"));
            *url = parsed.to_string();
        }
    }
    let token = resolve_auth_token(provider);
    let mut last_error = None;
    for (index, url) in urls.iter().enumerate() {
        let request = match token.as_deref() {
            Some(token) => client.get(url).bearer_auth(token),
            None => client.get(url),
        };
        let response = send_model_request(request, url).await?;
        let retry_candidate = matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) && index + 1 < urls.len();
        match parse_provider_response(response).await {
            Ok(value) => return Ok(value),
            Err(error) if retry_candidate => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "没有可用的模型查询地址".to_string()))
}

async fn send_model_request(
    request: reqwest::RequestBuilder,
    url: &str,
) -> std::result::Result<Response, String> {
    request
        .header(header::ACCEPT, "application/json")
        .header(
            header::USER_AGENT,
            concat!("codex-companion/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| request_error_message(&error, url))
}

fn relay_model_url_candidates(base_url: &str) -> std::result::Result<Vec<String>, String> {
    let base_url = provider_api_base_url(base_url);
    if base_url.is_empty() {
        return Err("Provider Base URL 为空".to_string());
    }
    let parsed = Url::parse(&base_url).map_err(|_| "Provider Base URL 无效".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Provider Base URL 必须使用 HTTP 或 HTTPS".to_string());
    }

    let base_url = base_url.trim_end_matches('/');
    let ends_with_version = parsed
        .path_segments()
        .and_then(Iterator::last)
        .is_some_and(is_version_segment);
    let mut candidates = if ends_with_version {
        vec![format!("{base_url}/models")]
    } else {
        vec![
            format!("{base_url}/v1/models"),
            format!("{base_url}/models"),
        ]
    };
    candidates.dedup();
    Ok(candidates)
}

fn is_version_segment(segment: &str) -> bool {
    segment.strip_prefix('v').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

async fn parse_provider_response(response: Response) -> std::result::Result<Value, String> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MODEL_RESPONSE_MAX_BYTES)
    {
        return Err("模型接口响应超过 8 MB 限制".to_string());
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取模型接口响应失败: {error}"))?
    {
        if bytes.len().saturating_add(chunk.len()) as u64 > MODEL_RESPONSE_MAX_BYTES {
            return Err("模型接口响应超过 8 MB 限制".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        if status.is_success() {
            format!("模型接口返回了无效 JSON: {error}")
        } else {
            format!("模型接口返回 HTTP {status}")
        }
    })?;
    if status.is_success() {
        Ok(value)
    } else {
        let detail = response_error_message(&value);
        Err(match detail {
            Some(detail) => format!("HTTP {status}: {detail}"),
            None => format!("模型接口返回 HTTP {status}"),
        })
    }
}

fn request_error_message(error: &reqwest::Error, url: &str) -> String {
    let endpoint = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "上游服务".to_string());
    if error.is_timeout() {
        format!("网络连接超时（{endpoint}）")
    } else if error.is_connect() {
        format!("网络连接失败（{endpoint}）")
    } else {
        format!("网络请求失败（{endpoint}）")
    }
}

fn response_error_message(value: &Value) -> Option<String> {
    [
        value.pointer("/error/message"),
        value.get("error"),
        value.get("message"),
        value.get("msg"),
        value.get("detail"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .map(|message| redact_sensitive_text(message).chars().take(180).collect())
}

fn parse_model_response(
    value: &Value,
    trusted_capabilities: bool,
) -> std::result::Result<Vec<DiscoveredModel>, String> {
    let entries = value
        .as_array()
        .or_else(|| value.get("models").and_then(Value::as_array))
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("items").and_then(Value::as_array))
        .or_else(|| value.pointer("/data/models").and_then(Value::as_array))
        .map(|entries| {
            entries
                .iter()
                .map(|entry| (None, entry))
                .collect::<Vec<_>>()
        })
        .or_else(|| {
            value
                .get("models")
                .and_then(Value::as_object)
                .map(|models| {
                    models
                        .iter()
                        .map(|(id, entry)| (Some(id.as_str()), entry))
                        .collect::<Vec<_>>()
                })
        })
        .ok_or_else(|| "模型接口响应中没有 models、data 或 items 集合".to_string())?;
    let mut models = BTreeMap::new();
    for (fallback_id, entry) in entries {
        let Some(model) = parse_model_entry(entry, fallback_id, trusted_capabilities) else {
            continue;
        };
        models.entry(model.id.clone()).or_insert(model);
    }
    Ok(models.into_values().collect())
}

fn parse_model_entry(
    entry: &Value,
    fallback_id: Option<&str>,
    trusted_capabilities: bool,
) -> Option<DiscoveredModel> {
    if let Some(id) = entry.as_str().and_then(non_empty) {
        return Some(DiscoveredModel {
            id: id.clone(),
            display_name: id,
            reasoning_efforts: Vec::new(),
            multi_agent_version: None,
            visibility: None,
            context_window: None,
            max_context_window: None,
            auto_compact_token_limit: None,
            priority: None,
            trusted_capabilities,
        });
    }
    let id = [
        entry.get("slug"),
        entry.get("id"),
        entry.get("model"),
        entry.get("name"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .and_then(non_empty)
    .or_else(|| fallback_id.and_then(non_empty))?;
    let mut normalized = entry.clone();
    if normalized.get("slug").and_then(Value::as_str).is_none() {
        normalized["slug"] = Value::String(id.clone());
    }
    if trusted_capabilities {
        codex_companion_core::repair_codex_model_metadata(&mut normalized);
    } else {
        codex_companion_core::repair_codex_context_metadata(&mut normalized);
    }
    let display_name = [normalized.get("display_name"), normalized.get("name")]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .and_then(non_empty)
        .unwrap_or_else(|| id.clone());
    let reasoning_efforts = normalized
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    level
                        .as_str()
                        .or_else(|| level.get("effort").and_then(Value::as_str))
                        .and_then(non_empty)
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(DiscoveredModel {
        id,
        display_name,
        reasoning_efforts,
        multi_agent_version: normalized
            .get("multi_agent_version")
            .and_then(Value::as_str)
            .and_then(non_empty),
        visibility: normalized
            .get("visibility")
            .and_then(Value::as_str)
            .and_then(non_empty),
        context_window: normalized.get("context_window").and_then(Value::as_i64),
        max_context_window: normalized.get("max_context_window").and_then(Value::as_i64),
        auto_compact_token_limit: normalized
            .get("auto_compact_token_limit")
            .and_then(Value::as_i64),
        priority: normalized.get("priority").and_then(Value::as_i64),
        trusted_capabilities,
    })
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn build_matrix(discoveries: Vec<SourceDiscovery>) -> ModelMatrixSnapshot {
    let source_order = discoveries
        .iter()
        .enumerate()
        .map(|(index, discovery)| (discovery.source.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut rows = BTreeMap::<String, ModelAccumulator>::new();
    for discovery in &discoveries {
        for model in &discovery.models {
            let row = rows
                .entry(model.id.clone())
                .or_insert_with(|| ModelAccumulator {
                    display_name: model.id.clone(),
                    source_ids: BTreeSet::new(),
                    reasoning_efforts: BTreeSet::new(),
                    multi_agent_version: None,
                    ultra_capable: false,
                    visibility: None,
                    context_window: None,
                    max_context_window: None,
                    auto_compact_token_limit: None,
                    priority: None,
                    trusted_name: false,
                });
            row.source_ids.insert(discovery.source.id.clone());
            if model.trusted_capabilities {
                if !row.trusted_name || row.display_name == model.id {
                    row.display_name = model.display_name.clone();
                    row.trusted_name = true;
                }
                row.reasoning_efforts
                    .extend(model.reasoning_efforts.iter().cloned());
                if model.multi_agent_version.as_deref() == Some("v2")
                    || row.multi_agent_version.is_none()
                {
                    row.multi_agent_version = model.multi_agent_version.clone();
                }
                row.ultra_capable |= model
                    .reasoning_efforts
                    .iter()
                    .any(|effort| effort == "ultra")
                    && model.multi_agent_version.as_deref() == Some("v2");
                if row.visibility.is_none() {
                    row.visibility = model.visibility.clone();
                }
                row.priority = match (row.priority, model.priority) {
                    (Some(current), Some(next)) => Some(current.min(next)),
                    (None, Some(next)) => Some(next),
                    (current, None) => current,
                };
            }
            if model.trusted_capabilities || row.context_window.is_none() {
                row.context_window = model.context_window;
            }
            if model.trusted_capabilities || row.max_context_window.is_none() {
                row.max_context_window = model.max_context_window;
            }
            if model.trusted_capabilities || row.auto_compact_token_limit.is_none() {
                row.auto_compact_token_limit = model.auto_compact_token_limit;
            }
        }
    }

    let mut models = rows
        .into_iter()
        .map(|(id, row)| {
            let mut source_ids = row.source_ids.into_iter().collect::<Vec<_>>();
            source_ids.sort_by_key(|source_id| source_order.get(source_id).copied());
            let mut reasoning_efforts = row.reasoning_efforts.into_iter().collect::<Vec<_>>();
            reasoning_efforts.sort_by(|left, right| {
                reasoning_effort_rank(left)
                    .cmp(&reasoning_effort_rank(right))
                    .then_with(|| left.cmp(right))
            });
            (
                row.priority,
                ModelMatrixModel {
                    id,
                    display_name: row.display_name,
                    source_ids,
                    reasoning_efforts,
                    multi_agent_version: row.multi_agent_version,
                    ultra_capable: row.ultra_capable,
                    visibility: row.visibility,
                    context_window: row.context_window,
                    max_context_window: row.max_context_window,
                    auto_compact_token_limit: row.auto_compact_token_limit,
                },
            )
        })
        .collect::<Vec<_>>();
    models.sort_by(|(left_priority, left), (right_priority, right)| {
        left_priority
            .unwrap_or(i64::MAX)
            .cmp(&right_priority.unwrap_or(i64::MAX))
            .then_with(|| left.id.cmp(&right.id))
    });

    ModelMatrixSnapshot {
        generated_at: Utc::now(),
        sources: discoveries
            .into_iter()
            .map(|discovery| discovery.source)
            .collect(),
        models: models.into_iter().map(|(_, model)| model).collect(),
    }
}

fn reasoning_effort_rank(effort: &str) -> usize {
    match effort {
        "minimal" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" => 4,
        "max" => 5,
        "ultra" => 6,
        _ => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn source(id: &str, kind: ModelSourceKind, models: Vec<DiscoveredModel>) -> SourceDiscovery {
        SourceDiscovery {
            source: ModelMatrixSource {
                id: id.to_string(),
                name: id.to_string(),
                kind,
                provider_id: None,
                active_group: false,
                status: ModelSourceStatus::Available,
                model_count: models.len(),
                fetched_at: None,
                error: None,
            },
            models,
        }
    }

    #[test]
    fn picker_discovers_provider_models_without_inventing_membership() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request");
            let mut request = [0; 1024];
            let size = stream.read(&mut request).expect("read request");
            assert!(String::from_utf8_lossy(&request[..size])
                .starts_with("GET /v1/models?client_version="));
            let body = json!({"data": [
                {"id": "gpt-6.1-sol"},
                {"id": "hidden-model", "visibility": "hide"},
                {"id": "blocked-model"}
            ]})
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        });
        let provider = ProviderConfig {
            id: "relay".into(),
            name: "Relay".into(),
            kind: ProviderKind::RelayProvider,
            base_url: format!("http://{address}/v1"),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        };
        let policy = AccountProtection {
            excluded_models: vec!["blocked-model".into()],
            ..Default::default()
        };

        assert_eq!(
            discover_relay_model_slugs(&[provider], &policy),
            vec!["gpt-6.1-sol"]
        );
        server.join().expect("server");
    }

    #[test]
    fn parses_official_and_openai_model_shapes() {
        let official = parse_model_response(
            &json!({"models": [{
                "slug": "gpt-test",
                "display_name": "GPT Test",
                "priority": 2,
                "supported_reasoning_levels": [{"effort": "high"}]
            }]}),
            true,
        )
        .expect("official models");
        let relay = parse_model_response(
            &json!({"object": "list", "data": [{"id": "relay-model"}]}),
            false,
        )
        .expect("relay models");

        assert_eq!(official[0].id, "gpt-test");
        assert_eq!(official[0].reasoning_efforts, vec!["high"]);
        assert_eq!(relay[0].id, "relay-model");
    }

    #[test]
    fn carries_codex_context_metadata_without_inventing_relay_capabilities() {
        let official = parse_model_response(&json!({"models": [{"slug": "gpt-6-sol"}]}), true)
            .expect("official models");
        assert_eq!(official[0].context_window, Some(256_000));
        assert_eq!(official[0].max_context_window, Some(256_000));
        assert_eq!(official[0].auto_compact_token_limit, Some(230_400));

        let relay = parse_model_response(
            &json!({"data": [{"id": "relay-model", "context_window": 64_000}]}),
            false,
        )
        .expect("relay models");
        assert_eq!(relay[0].context_window, Some(64_000));
        assert_eq!(relay[0].max_context_window, None);
        assert_eq!(relay[0].auto_compact_token_limit, None);

        let relay_only = build_matrix(vec![source("relay", ModelSourceKind::Relay, relay.clone())]);
        assert_eq!(relay_only.models[0].context_window, Some(64_000));

        let relay_codex = parse_model_response(
            &json!({"data": [{"id": "gpt-6-sol", "context_window": 64_000}]}),
            false,
        )
        .expect("relay codex model");
        let merged = build_matrix(vec![
            source("relay", ModelSourceKind::Relay, relay_codex),
            source("official", ModelSourceKind::OfficialOauth, official),
        ]);
        assert_eq!(merged.models[0].context_window, Some(256_000));
        assert_eq!(merged.models[0].auto_compact_token_limit, Some(230_400));
    }

    #[test]
    fn parses_items_and_model_map_shapes() {
        let items = parse_model_response(&json!({"items": [{"name": "items-model"}]}), false)
            .expect("items models");
        let model_map = parse_model_response(
            &json!({"models": {
                "mapped-model": {"display_name": "Mapped Model"},
                "string-entry": "string-model"
            }}),
            true,
        )
        .expect("model map");

        assert_eq!(items[0].id, "items-model");
        assert_eq!(model_map[0].id, "mapped-model");
        assert_eq!(model_map[0].display_name, "Mapped Model");
        assert_eq!(model_map[1].id, "string-model");
    }

    #[test]
    fn builds_standard_relay_model_url_candidates() {
        assert_eq!(
            relay_model_url_candidates("https://api.example.com/v1/responses")
                .expect("response endpoint"),
            vec!["https://api.example.com/v1/models"]
        );
        assert_eq!(
            relay_model_url_candidates("https://api.example.com").expect("bare base URL"),
            vec![
                "https://api.example.com/v1/models",
                "https://api.example.com/models"
            ]
        );
        assert_eq!(
            relay_model_url_candidates("https://api.example.com/api/v4")
                .expect("versioned base URL"),
            vec!["https://api.example.com/api/v4/models"]
        );
    }

    #[test]
    fn redacts_and_bounds_provider_error_messages() {
        let message = format!(
            "Authorization: Bearer upstream-secret refresh_token=refresh-secret {}",
            "x".repeat(256)
        );
        let error = response_error_message(&json!({
            "error": { "message": message }
        }))
        .expect("error message");

        assert!(!error.contains("upstream-secret"));
        assert!(!error.contains("refresh-secret"));
        assert!(error.chars().count() <= 180);
    }

    #[test]
    fn relay_metadata_never_claims_ultra_capability() {
        let relay = parse_model_response(
            &json!({"data": [{
                "id": "relay-only",
                "multi_agent_version": "v2",
                "supported_reasoning_levels": [{"effort": "ultra"}]
            }]}),
            false,
        )
        .expect("relay models");
        let matrix = build_matrix(vec![source("relay", ModelSourceKind::Relay, relay)]);

        assert!(!matrix.models[0].ultra_capable);
        assert!(matrix.models[0].reasoning_efforts.is_empty());
    }

    #[test]
    fn official_ultra_requires_multi_agent_v2() {
        let models = parse_model_response(
            &json!({"models": [
                {
                    "slug": "ultra-model",
                    "multi_agent_version": "v2",
                    "supported_reasoning_levels": [{"effort": "max"}, {"effort": "ultra"}]
                },
                {
                    "slug": "reasoning-only",
                    "supported_reasoning_levels": [{"effort": "ultra"}]
                }
            ]}),
            true,
        )
        .expect("official models");
        let matrix = build_matrix(vec![source(
            "official",
            ModelSourceKind::OfficialOauth,
            models,
        )]);

        assert!(
            matrix
                .models
                .iter()
                .find(|model| model.id == "ultra-model")
                .expect("ultra model")
                .ultra_capable
        );
        assert!(
            !matrix
                .models
                .iter()
                .find(|model| model.id == "reasoning-only")
                .expect("reasoning only")
                .ultra_capable
        );
    }
}
