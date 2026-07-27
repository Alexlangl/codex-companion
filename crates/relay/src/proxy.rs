use crate::content_encoding::{
    decode_request_body, RequestBodyDecodeError, MAX_REQUEST_BODY_BYTES,
};
use crate::events::{append_event, record_health_success, update_health};
use crate::state::{apply_group_policy, AffinityBindContext, RelayState};
use crate::upstream::{
    send_upstream, stream_response, text_response, upstream_url, UpstreamRequest,
};
use crate::{RequestLogFinish, RequestLogStart};
use axum::{
    body::Body,
    extract::{rejection::BytesRejection, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::{
    provider_endpoint_is_chat_completions, ApiClient, CompanionConfig, GroupPolicy,
    HealthFailureKind, HealthStatusKind, ProviderConfig, ProviderGroup, ProviderKind,
};
use codex_companion_health::{
    classify_failure, cooldown_active, mark_failure, mark_model_failure, normalize_expired_cooldown,
};
use codex_companion_provider::selected_providers_for_group;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) async fn proxy(
    State(state): State<RelayState>,
    method: Method,
    uri: Uri,
    mut headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return api_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "local_request_too_large",
                &format!(
                    "请求体超过 Codex Companion 本地代理的 {} MiB 上限；请先运行 /compact、移除大段日志或内联图片，或调整本地代理限制",
                    MAX_REQUEST_BODY_BYTES / (1024 * 1024)
                ),
            );
        }
        Err(error) => {
            return api_error_response(
                StatusCode::BAD_REQUEST,
                "request_body_read_failed",
                &format!("读取请求体失败: {error}"),
            );
        }
    };
    let body = match decode_request_body(&mut headers, body, MAX_REQUEST_BODY_BYTES) {
        Ok(body) => body,
        Err(RequestBodyDecodeError::TooLarge { .. }) => {
            return api_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "local_decompressed_request_too_large",
                &format!(
                    "解压后的请求体超过 Codex Companion 本地代理的 {} MiB 上限；请先运行 /compact、移除大段日志或内联图片，或调整本地代理限制",
                    MAX_REQUEST_BODY_BYTES / (1024 * 1024)
                ),
            );
        }
        Err(RequestBodyDecodeError::UnsupportedEncoding(encoding)) => {
            return api_error_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_content_encoding",
                &format!("不支持请求体编码 {encoding}"),
            );
        }
        Err(error @ RequestBodyDecodeError::InvalidEncoding { .. }) => {
            return api_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_content_encoding",
                &error.to_string(),
            );
        }
    };
    match proxy_inner(state, method, uri, headers, body).await {
        Ok(response) => response,
        Err(message) => api_error_response(
            StatusCode::BAD_GATEWAY,
            "proxy_internal_error",
            &compact_error_body(&message),
        ),
    }
}

async fn proxy_inner(
    state: RelayState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, String> {
    let request_id = next_request_id();
    let mut response = proxy_dispatch(state, method, uri, headers, body, &request_id)
        .await
        .unwrap_or_else(|message| {
            api_error_response(
                StatusCode::BAD_GATEWAY,
                "proxy_internal_error",
                &compact_error_body(&message),
            )
        });
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert("x-codex-companion-request-id", value);
    }
    Ok(response)
}

async fn proxy_dispatch(
    state: RelayState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    request_id: &str,
) -> std::result::Result<Response, String> {
    let started_at = Instant::now();
    append_event(
        &state.store,
        "request",
        None,
        format!("[{request_id}] {method} {uri}"),
    );
    let affinity_key = request_affinity_key(&headers, &body);
    let requested_model = request_model(&body);
    let mut config = state
        .store
        .load()
        .map_err(|error| format!("failed to load config: {error}"))?;
    let _ = state
        .api_service
        .prune_request_logs(config.relay.request_log_retention_days);
    if is_relay_root_probe(&method, &uri) {
        record_request_start(
            &state,
            request_id,
            &method,
            &uri,
            requested_model.as_deref(),
            None,
        );
        record_request_finish(
            &state,
            request_id,
            None,
            Some(StatusCode::OK),
            "local",
            0,
            started_at,
            None,
        );
        return Ok(relay_root_response());
    }
    let client = match authenticate_client(&state, &config, &headers) {
        Ok(client) => client,
        Err((status, message)) => {
            record_request_start(
                &state,
                request_id,
                &method,
                &uri,
                requested_model.as_deref(),
                None,
            );
            record_request_finish(
                &state,
                request_id,
                None,
                Some(status),
                "rejected",
                0,
                started_at,
                Some(&message),
            );
            append_event(
                &state.store,
                "error",
                None,
                format!("[{request_id}] {message}"),
            );
            return Ok(api_error_response(status, "invalid_api_key", &message));
        }
    };
    let affinity_key = affinity_key
        .map(|key| scoped_affinity_key(&key, client.as_ref().map(|client| client.id.as_str())));
    record_request_start(
        &state,
        request_id,
        &method,
        &uri,
        requested_model.as_deref(),
        client.as_ref().map(|client| client.id.as_str()),
    );
    if let (Some(client), Some(model)) = (client.as_ref(), requested_model.as_deref()) {
        if !client_allows_model(client, model) {
            let message = format!("API client {} 无权使用模型 {model}", client.name);
            record_request_finish(
                &state,
                request_id,
                None,
                Some(StatusCode::FORBIDDEN),
                "rejected",
                0,
                started_at,
                Some(&message),
            );
            return Ok(api_error_response(
                StatusCode::FORBIDDEN,
                "model_not_allowed",
                &message,
            ));
        }
    }
    if method == Method::GET && uri.path() == "/v1/models" {
        if let Some(client) = client
            .as_ref()
            .filter(|client| !client.allowed_models.is_empty())
        {
            record_request_finish(
                &state,
                request_id,
                None,
                Some(StatusCode::OK),
                "local",
                0,
                started_at,
                None,
            );
            return Ok(allowed_models_response(&client.allowed_models));
        }
    }
    normalize_health(&mut config);
    let group = config
        .groups
        .get(&config.relay.active_group_id)
        .cloned()
        .ok_or_else(|| format!("active group not found: {}", config.relay.active_group_id))?;
    let selected = selected_providers_for_group(&config, &group)
        .into_iter()
        .filter(|provider| provider.enabled)
        .collect::<Vec<_>>();
    if method == Method::GET
        && uri.path() == "/v1/models"
        && selected
            .iter()
            .any(|provider| provider.kind == ProviderKind::OfficialCodex)
    {
        let models = selected
            .iter()
            .flat_map(|provider| provider.model_map.keys().cloned())
            .filter(|model| model != "default")
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if !models.is_empty() {
            record_request_finish(
                &state,
                request_id,
                None,
                Some(StatusCode::OK),
                "local",
                0,
                started_at,
                None,
            );
            return Ok(allowed_models_response(&models));
        }
    }
    let affinity_preference = affinity_key.as_deref().and_then(|key| {
        state
            .preferred_provider(key, config.relay.session_affinity_ttl_seconds, &group)
            .map(|provider| (key, provider))
    });
    let priority_failback_claim = affinity_preference
        .as_ref()
        .and_then(|(key, _)| state.claim_priority_failback_probe(key, &group));
    let manually_requested_provider = priority_failback_claim
        .filter(|claim| claim.manual)
        .and_then(|_| {
            let (_, preference) = affinity_preference.as_ref()?;
            let target_provider = group.priority_failback_target_provider_id.as_deref()?;
            specific_higher_priority_provider(
                &group,
                &selected,
                &preference.provider_id,
                target_provider,
            )
        });
    // AuthFailed(key 被吊销/凭证失效)必须无条件排除：它不会随冷却到期恢复，
    // 只有刷新成功(mark_success)才解除；单账号/关闭 fallback 时也不能拿它无
    // 限重试。临时冷却(429/5xx)则只在有其他候选时跳过——只剩一个账号时拒绝
    // 尝试会把瞬时故障放大成全量 503。
    let has_alternatives = group.fallback_enabled && selected.len() > 1;
    let mut candidates = selected
        .into_iter()
        .filter(|provider| {
            let health = config.health.get(&provider.id);
            if health.is_some_and(|health| matches!(health.status, HealthStatusKind::AuthFailed)) {
                return false;
            }
            if manually_requested_provider
                .as_deref()
                .is_some_and(|provider_id| provider_id == provider.id)
            {
                return true;
            }
            if !has_alternatives {
                return true;
            }
            let globally_available = health.is_none_or(|health| !cooldown_active(health));
            let model_available = requested_model.as_deref().is_none_or(|model| {
                !state
                    .api_service
                    .model_cooldown_active(&provider.id, model)
                    .unwrap_or(false)
            });
            globally_available && model_available
        })
        .collect::<Vec<_>>();

    apply_group_policy(&state, &group, &mut candidates);

    // 会话亲和与向上探测都必须在 retry_budget 截断之前排序，确保探测失败时
    // 当前已知可用的 Provider 紧跟其后，不会被截断。
    let priority_probe_provider = affinity_preference.as_ref().and_then(|(_, preference)| {
        let automatic_probe_provider = priority_failback_claim
            .filter(|claim| claim.automatic)
            .and_then(|_| {
                nearest_higher_priority_provider(&group, &candidates, &preference.provider_id)
            });
        manually_requested_provider
            .filter(|provider_id| {
                candidates
                    .iter()
                    .any(|provider| provider.id == *provider_id)
            })
            .or(automatic_probe_provider)
    });
    if let Some((_, preference)) = affinity_preference.as_ref() {
        prioritize_session_affinity(
            &mut candidates,
            &preference.provider_id,
            priority_probe_provider.as_deref(),
        );
        if let Some(provider_id) = priority_probe_provider.as_ref() {
            let trigger = if priority_failback_claim.is_some_and(|claim| claim.manual)
                && group.priority_failback_target_provider_id.as_deref()
                    == Some(provider_id.as_str())
            {
                "手动"
            } else {
                "自动"
            };
            append_event(
                &state.store,
                "failback",
                Some(provider_id.clone()),
                format!("[{request_id}] 会话{trigger}向上探测 Provider {provider_id}"),
            );
        }
    }
    if !group.fallback_enabled {
        candidates.truncate(1);
    }
    if config.relay.retry_budget > 0 {
        candidates.truncate(usize::from(config.relay.retry_budget).saturating_add(1));
    }
    if candidates.is_empty() {
        let message = "当前本地代理分组没有可用账号".to_string();
        append_event(
            &state.store,
            "error",
            None,
            format!("[{request_id}] {message}"),
        );
        record_request_finish(
            &state,
            request_id,
            None,
            Some(StatusCode::SERVICE_UNAVAILABLE),
            "failed",
            0,
            started_at,
            Some(&message),
        );
        return Ok(api_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_available_provider",
            &message,
        ));
    }

    let mut last_error = None;
    let candidate_count = candidates.len();
    let compact_request = method == Method::POST && uri.path().ends_with("/responses/compact");
    for (index, provider) in candidates.into_iter().enumerate() {
        if compact_request && provider_endpoint_is_chat_completions(&provider.base_url) {
            let message = format!(
                "Provider {} 仅支持 Chat Completions，无法处理 Responses Compact API",
                provider.name
            );
            last_error = Some(message.clone());
            if index + 1 < candidate_count && group.fallback_enabled {
                append_event(
                    &state.store,
                    "fallback",
                    Some(provider.id),
                    format!("[{request_id}] {message}"),
                );
                continue;
            }
            record_request_finish(
                &state,
                request_id,
                Some(&provider.id),
                Some(StatusCode::NOT_IMPLEMENTED),
                "failed",
                (index + 1) as u16,
                started_at,
                Some(&message),
            );
            return Ok(api_error_response(
                StatusCode::NOT_IMPLEMENTED,
                "responses_compact_unsupported",
                &message,
            ));
        }
        let request_guard = state.begin_provider_request(&provider.id);
        let upstream = upstream_url(&provider, &uri);
        match send_upstream(
            &state.client,
            &state.api_service,
            UpstreamRequest::new(&provider, &method, &uri, &headers, body.clone(), &upstream),
        )
        .await
        {
            Ok(mut response) if response.status().is_success() => {
                let upstream_status = response.status();
                if let Err(error) = response.preflight_stream_failure().await {
                    let failure = classify_failure(None, &error);
                    let message = format!("stream 输出前语义失败: {}", compact_error_body(&error));
                    record_provider_failure(
                        &state,
                        &config,
                        &provider.id,
                        requested_model.as_deref(),
                        &failure,
                        &message,
                    );
                    last_error = Some(message.clone());
                    let can_retry = fallback_eligible(&failure)
                        && index + 1 < candidate_count
                        && group.fallback_enabled;
                    if can_retry {
                        append_event(
                            &state.store,
                            "fallback",
                            Some(provider.id),
                            format!("[{request_id}] {message}"),
                        );
                        continue;
                    }
                    record_request_finish(
                        &state,
                        request_id,
                        Some(&provider.id),
                        Some(StatusCode::BAD_GATEWAY),
                        "failed",
                        (index + 1) as u16,
                        started_at,
                        Some(&message),
                    );
                    return Ok(api_error_response(
                        StatusCode::BAD_GATEWAY,
                        failure_error_code(&failure.kind),
                        &message,
                    ));
                }
                let downstream = stream_response(
                    state.store.clone(),
                    request_id.to_string(),
                    provider.id.clone(),
                    response,
                )
                .await;
                let downstream_status = downstream.status();
                if downstream_status.is_success() {
                    if let Some(affinity_key) = affinity_key.as_deref() {
                        let priority_probe_generation = priority_probe_provider
                            .as_deref()
                            .filter(|provider_id| *provider_id == provider.id.as_str())
                            .and_then(|_| {
                                priority_failback_claim.map(|claim| claim.probe_generation)
                            });
                        state.bind_provider(
                            affinity_key,
                            &provider.id,
                            config.relay.session_affinity_ttl_seconds,
                            &group,
                            AffinityBindContext {
                                expected_route_generation: affinity_preference
                                    .as_ref()
                                    .map(|(_, preference)| preference.route_generation),
                                priority_probe_generation,
                            },
                        );
                    }
                    if let Some(model) = requested_model.as_deref() {
                        let _ = state.api_service.clear_model_cooldown(&provider.id, model);
                    }
                    record_health_success(&state.store, &provider.id);
                    append_event(
                        &state.store,
                        "stream",
                        Some(provider.id.clone()),
                        format!("[{request_id}] {method} {uri} -> {upstream_status}"),
                    );
                    record_request_finish(
                        &state,
                        request_id,
                        Some(&provider.id),
                        Some(downstream_status),
                        "succeeded",
                        (index + 1) as u16,
                        started_at,
                        None,
                    );
                    // 守卫随响应体流存活，LeastLoaded 才统计得到流式生成期间的真实负载。
                    return Ok(attach_request_guard(downstream, request_guard));
                }

                let message = format!("上游成功响应无法转换为本地协议（HTTP {downstream_status}）");
                let failure = classify_failure(Some(downstream_status.as_u16()), &message);
                record_provider_failure(
                    &state,
                    &config,
                    &provider.id,
                    requested_model.as_deref(),
                    &failure,
                    &message,
                );
                append_event(
                    &state.store,
                    "error",
                    Some(provider.id.clone()),
                    format!("[{request_id}] {message}"),
                );
                record_request_finish(
                    &state,
                    request_id,
                    Some(&provider.id),
                    Some(downstream_status),
                    "failed",
                    (index + 1) as u16,
                    started_at,
                    Some(&message),
                );
                return Ok(api_error_response(
                    downstream_status,
                    "response_transform_failed",
                    &message,
                ));
            }
            Ok(response) => {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                let failure = classify_failure(Some(status.as_u16()), &body_text);
                let upstream_payload_too_large = status == StatusCode::PAYLOAD_TOO_LARGE;
                let compact_unsupported = compact_request
                    && matches!(
                        status,
                        StatusCode::NOT_FOUND
                            | StatusCode::METHOD_NOT_ALLOWED
                            | StatusCode::NOT_IMPLEMENTED
                    );
                let message = if upstream_payload_too_large {
                    format!(
                        "上游 Provider {} 返回 413 Payload Too Large：请求体超过其服务端限制，并非 Codex Companion 本地限制；请运行 /compact、移除大段日志或内联图片，或联系 Provider 调高限制",
                        provider.name
                    )
                } else if compact_unsupported {
                    format!(
                        "上游 Provider {} 不支持 Responses Compact API（HTTP {status}）",
                        provider.name
                    )
                } else {
                    format!("上游返回 {}: {}", status, compact_error_body(&body_text))
                };
                if !upstream_payload_too_large && !compact_unsupported {
                    record_provider_failure(
                        &state,
                        &config,
                        &provider.id,
                        requested_model.as_deref(),
                        &failure,
                        &message,
                    );
                }
                last_error = Some(message.clone());
                let can_retry = (upstream_payload_too_large
                    || compact_unsupported
                    || fallback_eligible(&failure))
                    && index + 1 < candidate_count
                    && group.fallback_enabled;
                if can_retry {
                    append_event(
                        &state.store,
                        "fallback",
                        Some(provider.id),
                        format!("[{request_id}] {message}"),
                    );
                    continue;
                }
                append_event(
                    &state.store,
                    "error",
                    Some(provider.id.clone()),
                    format!("[{request_id}] {message}"),
                );
                record_request_finish(
                    &state,
                    request_id,
                    Some(&provider.id),
                    Some(status),
                    "failed",
                    (index + 1) as u16,
                    started_at,
                    Some(&message),
                );
                return Ok(api_error_response(
                    status,
                    if upstream_payload_too_large {
                        "upstream_request_too_large"
                    } else if compact_unsupported {
                        "responses_compact_unsupported"
                    } else {
                        failure_error_code(&failure.kind)
                    },
                    &message,
                ));
            }
            Err(error) => {
                let failure = classify_failure(None, &error);
                let message = format!("stream 开始前请求失败: {}", compact_error_body(&error));
                record_provider_failure(
                    &state,
                    &config,
                    &provider.id,
                    requested_model.as_deref(),
                    &failure,
                    &message,
                );
                last_error = Some(message.clone());
                let can_retry = fallback_eligible(&failure)
                    && index + 1 < candidate_count
                    && group.fallback_enabled;
                if can_retry {
                    append_event(
                        &state.store,
                        "fallback",
                        Some(provider.id),
                        format!("[{request_id}] {message}"),
                    );
                    continue;
                }
                append_event(
                    &state.store,
                    "error",
                    Some(provider.id),
                    format!("[{request_id}] {message}"),
                );
            }
        }
    }

    if let Some(error) = last_error.as_ref() {
        append_event(
            &state.store,
            "error",
            None,
            format!("[{request_id}] {error}"),
        );
    }
    let error = last_error.unwrap_or_else(|| "all providers failed".to_string());
    record_request_finish(
        &state,
        request_id,
        None,
        Some(StatusCode::BAD_GATEWAY),
        "failed",
        candidate_count as u16,
        started_at,
        Some(&error),
    );
    Ok(api_error_response(
        StatusCode::BAD_GATEWAY,
        "all_providers_failed",
        &error,
    ))
}

fn nearest_higher_priority_provider(
    group: &ProviderGroup,
    candidates: &[ProviderConfig],
    preferred_provider: &str,
) -> Option<String> {
    if !matches!(group.policy, GroupPolicy::PriorityFallback) || !group.fallback_enabled {
        return None;
    }
    let preferred_index = group
        .provider_order
        .iter()
        .position(|provider_id| provider_id == preferred_provider)?;
    candidates
        .iter()
        .filter_map(|provider| {
            let index = group
                .provider_order
                .iter()
                .position(|provider_id| provider_id == &provider.id)?;
            (index < preferred_index).then_some((index, provider.id.clone()))
        })
        .max_by_key(|(index, _)| *index)
        .map(|(_, provider_id)| provider_id)
}

fn specific_higher_priority_provider(
    group: &ProviderGroup,
    candidates: &[ProviderConfig],
    preferred_provider: &str,
    target_provider: &str,
) -> Option<String> {
    if !matches!(group.policy, GroupPolicy::PriorityFallback) || !group.fallback_enabled {
        return None;
    }
    let preferred_index = group
        .provider_order
        .iter()
        .position(|provider_id| provider_id == preferred_provider)?;
    let target_index = group
        .provider_order
        .iter()
        .position(|provider_id| provider_id == target_provider)?;
    if target_index >= preferred_index {
        return None;
    }
    candidates
        .iter()
        .find(|provider| provider.id == target_provider)
        .map(|provider| provider.id.clone())
}

fn prioritize_session_affinity(
    candidates: &mut Vec<ProviderConfig>,
    preferred_provider: &str,
    priority_probe_provider: Option<&str>,
) {
    let priority_probe = priority_probe_provider.and_then(|provider_id| {
        candidates
            .iter()
            .position(|provider| provider.id == provider_id)
            .map(|index| candidates.remove(index))
    });
    let preferred = candidates
        .iter()
        .position(|provider| provider.id == preferred_provider)
        .map(|index| candidates.remove(index));
    if let Some(priority_probe) = priority_probe {
        candidates.insert(0, priority_probe);
        if let Some(preferred) = preferred {
            candidates.insert(1, preferred);
        }
    } else if let Some(preferred) = preferred {
        candidates.insert(0, preferred);
    }
}

fn fallback_eligible(failure: &codex_companion_health::FailureClassification) -> bool {
    failure.retryable || failure.kind == HealthFailureKind::AuthFailed
}

fn failure_error_code(kind: &HealthFailureKind) -> &'static str {
    match kind {
        HealthFailureKind::AuthFailed => "upstream_auth_failed",
        HealthFailureKind::RateLimited => "upstream_rate_limited",
        HealthFailureKind::QuotaExhausted => "upstream_quota_exhausted",
        HealthFailureKind::ModelMissing => "upstream_model_not_found",
        HealthFailureKind::NetworkFailed => "upstream_network_error",
        HealthFailureKind::UpstreamFailed => "upstream_error",
        HealthFailureKind::Unknown => "upstream_unknown_error",
    }
}

fn authenticate_client(
    state: &RelayState,
    config: &CompanionConfig,
    headers: &HeaderMap,
) -> std::result::Result<Option<ApiClient>, (StatusCode, String)> {
    let token = client_api_key(headers);
    let client = token
        .as_deref()
        .map(|token| state.api_service.authenticate(token))
        .transpose()
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("API client 数据库不可用: {error}"),
            )
        })?
        .flatten();
    let browser_origin = headers.contains_key(header::ORIGIN);
    if client.is_none() && (config.relay.require_api_key || browser_origin) {
        let message = if token.is_some() {
            "API key 无效、已停用或已轮换".to_string()
        } else {
            "此本地 API 服务需要 Authorization: Bearer <API_KEY>".to_string()
        };
        return Err((StatusCode::UNAUTHORIZED, message));
    }
    Ok(client)
}

fn client_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        })
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn attach_request_guard(response: Response, guard: crate::state::ProviderRequestGuard) -> Response {
    let (parts, body) = response.into_parts();
    let stream = body.into_data_stream().map(move |chunk| {
        let _ = &guard;
        chunk
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

fn client_allows_model(client: &ApiClient, model: &str) -> bool {
    client.allowed_models.is_empty() || client.allowed_models.iter().any(|allowed| allowed == model)
}

fn request_model(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}

fn is_model_scoped_failure(kind: &HealthFailureKind) -> bool {
    matches!(
        kind,
        HealthFailureKind::ModelMissing | HealthFailureKind::RateLimited
    )
}

fn record_provider_failure(
    state: &RelayState,
    config: &CompanionConfig,
    provider_id: &str,
    model: Option<&str>,
    failure: &codex_companion_health::FailureClassification,
    message: &str,
) {
    if let Some(model) = model.filter(|_| is_model_scoped_failure(&failure.kind)) {
        let _ = state.api_service.set_model_cooldown(
            provider_id,
            model,
            message,
            config.relay.model_cooldown_seconds,
        );
        update_health(&state.store, provider_id, |health| {
            mark_model_failure(health, failure, message.to_string())
        });
    } else {
        update_health(&state.store, provider_id, |health| {
            mark_failure(health, failure, message.to_string())
        });
    }
}

fn record_request_start(
    state: &RelayState,
    request_id: &str,
    method: &Method,
    uri: &Uri,
    model: Option<&str>,
    client_id: Option<&str>,
) {
    let _ = state.api_service.record_request_start(RequestLogStart {
        request_id,
        method: method.as_str(),
        path: uri
            .path_and_query()
            .map_or(uri.path(), |value| value.as_str()),
        model,
        client_id,
    });
}

#[allow(clippy::too_many_arguments)]
fn record_request_finish(
    state: &RelayState,
    request_id: &str,
    provider_id: Option<&str>,
    status: Option<StatusCode>,
    outcome: &str,
    attempts: u16,
    started_at: Instant,
    error: Option<&str>,
) {
    let _ = state.api_service.record_request_finish(RequestLogFinish {
        request_id,
        provider_id,
        status_code: status.map(|status| status.as_u16()),
        outcome,
        attempts,
        latency_ms: started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        error,
    });
}

fn api_error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "codex_companion_error",
            "code": code
        }
    });
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        )
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| text_response(status, message.to_string()))
}

fn allowed_models_response(models: &[String]) -> Response {
    let data = models
        .iter()
        .map(|model| serde_json::json!({"id": model, "object": "model", "owned_by": "codex-companion"}))
        .collect::<Vec<_>>();
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        )
        .body(Body::from(
            serde_json::json!({"object": "list", "data": data}).to_string(),
        ))
        .expect("models response")
}

fn next_request_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("cc-{timestamp:x}-{sequence:x}")
}

fn request_affinity_key(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    for header in [
        "session_id",
        "x-session-id",
        "x-client-request-id",
        "x-amp-thread-id",
    ] {
        if let Some(value) = headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(hash_affinity_key(&format!("header:{header}={value}")));
        }
    }

    let value = serde_json::from_slice::<Value>(body).ok()?;
    for path in [
        &["metadata", "session_id"][..],
        &["metadata", "user_id"][..],
        &["conversation_id"][..],
        &["thread_id"][..],
        &["prompt_cache_key"][..],
    ] {
        if let Some(value) = json_string_at_path(&value, path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(hash_affinity_key(&format!(
                "body:{}={value}",
                path.join(".")
            )));
        }
    }
    None
}

fn json_string_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

fn hash_affinity_key(value: &str) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn scoped_affinity_key(key: &str, client_id: Option<&str>) -> String {
    hash_affinity_key(&format!("client:{}:{key}", client_id.unwrap_or("local")))
}

fn normalize_health(config: &mut CompanionConfig) {
    for health in config.health.values_mut() {
        normalize_expired_cooldown(health);
    }
}

fn compact_error_body(body: &str) -> String {
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 280 {
        format!("{}...", text.chars().take(280).collect::<String>())
    } else if text.is_empty() {
        "无响应正文".to_string()
    } else {
        text
    }
}

fn is_relay_root_probe(method: &Method, uri: &Uri) -> bool {
    method == Method::GET && matches!(uri.path(), "/" | "/v1")
}

fn relay_root_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        )
        .body(Body::from(
            r#"{"object":"codex_companion.relay","status":"ok"}"#,
        ))
        .expect("response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, routing::any, Router};
    use chrono::{Duration as ChronoDuration, Utc};
    use codex_companion_core::{
        ApiClientCreate, ConfigStore, GroupPolicy, HealthFailureKind, ProviderConfig,
        ProviderGroup, ProviderHealth, ProviderKind,
    };
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn relay_root_probe_is_handled_locally() {
        let root: Uri = "/v1".parse().expect("uri");
        let models: Uri = "/v1/models".parse().expect("uri");

        assert!(is_relay_root_probe(&Method::GET, &root));
        assert!(!is_relay_root_probe(&Method::POST, &root));
        assert!(!is_relay_root_probe(&Method::GET, &models));
    }

    #[tokio::test]
    async fn official_codex_models_are_served_from_the_validated_local_catalog() {
        let store = store_with_group(vec![provider(
            "official",
            "https://chatgpt.com/backend-api/codex",
        )]);
        store
            .update(|config| {
                let provider = config.providers.get_mut("official").expect("provider");
                provider.kind = ProviderKind::OfficialCodex;
                provider
                    .model_map
                    .insert("gpt-live".to_string(), "gpt-live".to_string());
                Ok(())
            })
            .expect("update provider");

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("models");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["object"], "list");
        assert_eq!(value["data"][0]["id"], "gpt-live");
    }

    #[tokio::test]
    async fn falls_back_to_next_provider_before_stream_starts() {
        let provider_a_url =
            spawn_mock_server(StatusCode::INTERNAL_SERVER_ERROR, "upstream failed", None).await;
        let provider_b_url = spawn_mock_server(StatusCode::OK, "ok from b", None).await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("b")
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"ok from b");
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn falls_back_when_upstream_rejects_request_body_as_too_large() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(
            StatusCode::PAYLOAD_TOO_LARGE,
            "<html>request too large</html>",
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"object":"list","data":[]}"#,
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("small-gateway", &provider_a_url),
            provider("large-gateway", &provider_b_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("large-gateway")
        );
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        assert!(!store
            .load()
            .expect("config")
            .health
            .contains_key("small-gateway"));
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("上游 Provider small-gateway 返回 413"));
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn returns_actionable_error_for_single_upstream_413() {
        let provider_url = spawn_mock_server(
            StatusCode::PAYLOAD_TOO_LARGE,
            "<html>request too large</html>",
            None,
        )
        .await;
        let store = store_with_group(vec![provider("small-gateway", &provider_url)]);

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["error"]["code"], "upstream_request_too_large");
        assert!(value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("并非 Codex Companion 本地限制")));
        assert!(value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("/compact")));
    }

    #[tokio::test]
    async fn compact_skips_chat_only_provider_and_uses_responses_provider() {
        let chat_hits = Arc::new(AtomicUsize::new(0));
        let responses_hits = Arc::new(AtomicUsize::new(0));
        let chat_url = spawn_mock_server(
            StatusCode::OK,
            "should not be called",
            Some(chat_hits.clone()),
        )
        .await;
        let responses_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"object":"response.compaction","output":[]}"#,
            Some(responses_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("chat-only", &format!("{chat_url}/chat/completions")),
            provider("responses", &responses_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses/compact".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":[]}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("responses")
        );
        assert_eq!(chat_hits.load(Ordering::SeqCst), 0);
        assert_eq!(responses_hits.load(Ordering::SeqCst), 1);
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("无法处理 Responses Compact API"));
    }

    #[tokio::test]
    async fn compact_falls_back_when_provider_does_not_expose_compact_endpoint() {
        let unsupported_hits = Arc::new(AtomicUsize::new(0));
        let responses_hits = Arc::new(AtomicUsize::new(0));
        let unsupported_url = spawn_mock_server(
            StatusCode::NOT_FOUND,
            r#"{"error":{"message":"endpoint not found"}}"#,
            Some(unsupported_hits.clone()),
        )
        .await;
        let responses_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"object":"response.compaction","output":[]}"#,
            Some(responses_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("responses-without-compact", &unsupported_url),
            provider("responses-with-compact", &responses_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses/compact".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":[]}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("responses-with-compact")
        );
        assert_eq!(unsupported_hits.load(Ordering::SeqCst), 1);
        assert_eq!(responses_hits.load(Ordering::SeqCst), 1);
        assert!(!store
            .load()
            .expect("config")
            .health
            .contains_key("responses-without-compact"));
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("不支持 Responses Compact API"));
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn falls_back_when_one_account_authentication_has_expired() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"expired token"}}"#,
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"object":"list","data":[]}"#,
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("expired", &provider_a_url),
            provider("healthy", &provider_b_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("healthy")
        );
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        let health = store.load().expect("config").health;
        assert_eq!(
            health
                .get("expired")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::AuthFailed)
        );
    }

    #[tokio::test]
    async fn success_status_does_not_probe_next_provider() {
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(StatusCode::OK, "stream from a", None).await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            "should not be called",
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState::new(store, reqwest::Client::new());

        let response = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("a")
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"stream from a");
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn falls_back_when_responses_upstream_returns_semantic_failure_in_200() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"status":"failed","error":{"message":"temporarily overloaded"}}"#,
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"chatcmpl_b","model":"gpt-test","choices":[{"message":{"role":"assistant","content":"ok from b"}}]}"#,
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &format!("{provider_a_url}/chat/completions")),
            provider("b", &format!("{provider_b_url}/chat/completions")),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state,
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","stream":false}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("b")
        );
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        assert!(String::from_utf8_lossy(&body).contains("ok from b"));
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("upstream semantic failure"));
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn falls_back_when_sse_fails_before_any_output() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_a\"}}\n\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",",
                "\"error\":{\"message\":\"overloaded before output\"}}}\n\n"
            ),
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok from b\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_b\",",
                "\"status\":\"completed\"}}\n\n"
            ),
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","stream":true}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("b")
        );
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("ok from b"));
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_fallback_after_sse_output_has_started() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial from a\"}\n\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",",
                "\"error\":{\"message\":\"failed after output\"}}}\n\n"
            ),
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_sse_mock_server(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","stream":true}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("a")
        );
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("partial from a"));
        assert!(body.contains("response.failed"));
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transforms_sse_even_when_upstream_content_type_is_wrong() {
        let provider_url = spawn_mock_server(
            StatusCode::OK,
            concat!(
                "data: {\"id\":\"chatcmpl_sse\",\"model\":\"gpt-test\",",
                "\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
            None,
        )
        .await;
        let store = store_with_group(vec![provider(
            "a",
            &format!("{provider_url}/chat/completions"),
        )]);
        let state = RelayState::new(store, reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","stream":true}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream; charset=utf-8")
        );
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("你好"));
        assert!(body.contains("response.completed"));
    }

    #[test]
    fn affinity_key_accepts_metadata_session_id_but_not_previous_response_id() {
        let session = request_affinity_key(
            &HeaderMap::new(),
            br#"{"metadata":{"session_id":"session-123"}}"#,
        );
        assert!(session.is_some());
        assert_eq!(
            session,
            request_affinity_key(
                &HeaderMap::new(),
                br#"{"metadata":{"session_id":"session-123"},"previous_response_id":"resp-2"}"#,
            )
        );
        assert!(
            request_affinity_key(&HeaderMap::new(), br#"{"previous_response_id":"resp-1"}"#,)
                .is_none()
        );
    }

    #[test]
    fn affinity_key_is_isolated_per_api_client() {
        let raw = hash_affinity_key("session:shared");
        assert_ne!(
            scoped_affinity_key(&raw, Some("client-a")),
            scoped_affinity_key(&raw, Some("client-b"))
        );
        assert_eq!(
            scoped_affinity_key(&raw, Some("client-a")),
            scoped_affinity_key(&raw, Some("client-a"))
        );
    }

    #[tokio::test]
    async fn session_affinity_survives_group_reordering() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url =
            spawn_mock_server(StatusCode::OK, "from a", Some(provider_a_hits.clone())).await;
        let provider_b_url =
            spawn_mock_server(StatusCode::OK, "from b", Some(provider_b_hits.clone())).await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let mut session_headers = HeaderMap::new();
        session_headers.insert("x-session-id", HeaderValue::from_static("thread-1"));

        let first = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("first request");
        assert!(first.headers().contains_key("x-codex-companion-request-id"));
        assert_eq!(
            &to_bytes(first.into_body(), 1024).await.expect("first body")[..],
            b"from a"
        );

        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["b".to_string(), "a".to_string()];
                Ok(())
            })
            .expect("reorder group");

        let sticky = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers,
            Bytes::new(),
        )
        .await
        .expect("sticky request");
        assert_eq!(
            &to_bytes(sticky.into_body(), 1024)
                .await
                .expect("sticky body")[..],
            b"from a"
        );

        let mut other_session = HeaderMap::new();
        other_session.insert("x-session-id", HeaderValue::from_static("thread-2"));
        let unbound = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            other_session,
            Bytes::new(),
        )
        .await
        .expect("unbound request");
        assert_eq!(
            &to_bytes(unbound.into_body(), 1024)
                .await
                .expect("unbound body")[..],
            b"from b"
        );
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 2);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn incomplete_stream_marks_provider_unhealthy_with_same_request_id() {
        let provider_url = spawn_mock_server(
            StatusCode::OK,
            concat!(
                "data: {\"id\":\"chatcmpl_cut\",\"model\":\"gpt-test\",",
                "\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n"
            ),
            None,
        )
        .await;
        let store = store_with_group(vec![provider(
            "a",
            &format!("{provider_url}/chat/completions"),
        )]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello","stream":true}"#),
        )
        .await
        .expect("proxy");
        let request_id = response
            .headers()
            .get("x-codex-companion-request-id")
            .and_then(|value| value.to_str().ok())
            .expect("request id")
            .to_string();
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("response.failed"));

        let config = store.load().expect("config");
        let health = config.health.get("a").expect("provider health");
        assert_eq!(
            health.last_failure_kind,
            Some(HealthFailureKind::UpstreamFailed)
        );
        assert!(health.cooldown_until.is_some());
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains(&request_id));
        assert!(events.contains("upstream_stream_incomplete"));
        let request = state
            .api_service
            .snapshot(10)
            .expect("request log")
            .recent_requests
            .into_iter()
            .find(|request| request.request_id == request_id)
            .expect("stream request");
        assert_eq!(request.outcome, "failed");
    }

    #[tokio::test]
    async fn single_provider_route_ignores_its_own_cooldown() {
        let provider_hits = Arc::new(AtomicUsize::new(0));
        let provider_url = spawn_mock_server(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporary unavailable",
            Some(provider_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &provider_url)]);
        store
            .update(|config| {
                let failure = classify_failure(Some(503), "temporary unavailable");
                let health = config
                    .health
                    .entry("a".to_string())
                    .or_insert_with(ProviderHealth::default);
                mark_failure(health, &failure, "temporary unavailable".to_string());
                Ok(())
            })
            .expect("seed cooldown");
        let state = RelayState::new(store, reqwest::Client::new());

        let response = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(provider_hits.load(Ordering::SeqCst), 1);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["error"]["code"], "upstream_error");
        assert!(value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("temporary unavailable")));
    }

    #[tokio::test]
    async fn strict_client_key_and_model_allowlist_are_enforced_before_upstream() {
        let hits = Arc::new(AtomicUsize::new(0));
        let provider_url =
            spawn_mock_server(StatusCode::OK, r#"{"status":"ok"}"#, Some(hits.clone())).await;
        let store = store_with_group(vec![provider("a", &provider_url)]);
        store
            .update(|config| {
                config.relay.require_api_key = true;
                Ok(())
            })
            .expect("strict mode");
        let state = RelayState::new(store, reqwest::Client::new());
        let secret = state
            .api_service
            .create_client(ApiClientCreate {
                name: "test client".to_string(),
                allowed_models: vec!["gpt-allowed".to_string()],
            })
            .expect("client");

        let missing = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-allowed","input":"hello"}"#),
        )
        .await
        .expect("missing key");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", secret.api_key)).expect("header"),
        );
        let denied = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            headers.clone(),
            Bytes::from_static(br#"{"model":"gpt-denied","input":"hello"}"#),
        )
        .await
        .expect("denied model");
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        let allowed = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            headers,
            Bytes::from_static(br#"{"model":"gpt-allowed","input":"hello"}"#),
        )
        .await
        .expect("allowed model");
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let snapshot = state.api_service.snapshot(10).expect("snapshot");
        assert_eq!(snapshot.recent_requests.len(), 3);
        assert!(snapshot
            .recent_requests
            .iter()
            .any(|request| request.client_name.as_deref() == Some("test client")));
    }

    #[tokio::test]
    async fn retry_budget_caps_provider_attempts() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::SERVICE_UNAVAILABLE,
            "a failed",
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(
            StatusCode::SERVICE_UNAVAILABLE,
            "b failed",
            Some(hits_b.clone()),
        )
        .await;
        let url_c =
            spawn_mock_server(StatusCode::OK, "c should not run", Some(hits_c.clone())).await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        store
            .update(|config| {
                config.relay.retry_budget = 1;
                Ok(())
            })
            .expect("budget");

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert_eq!(hits_c.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rate_limit_cools_only_the_failed_provider_model_pair() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit",
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(StatusCode::OK, "ok", Some(hits_b.clone())).await;
        let store = store_with_group(vec![provider("a", &url_a), provider("b", &url_b)]);
        let state = RelayState::new(store, reqwest::Client::new());

        for model in ["gpt-one", "gpt-one", "gpt-two"] {
            let response = proxy_inner(
                state.clone(),
                Method::POST,
                "/v1/responses".parse().expect("uri"),
                HeaderMap::new(),
                Bytes::from(format!(r#"{{"model":"{model}","input":"hello"}}"#)),
            )
            .await
            .expect("proxy");
            assert_eq!(response.status(), StatusCode::OK);
        }

        assert_eq!(hits_a.load(Ordering::SeqCst), 2);
        assert_eq!(hits_b.load(Ordering::SeqCst), 3);
        assert!(state
            .api_service
            .model_cooldown_active("a", "gpt-one")
            .expect("gpt-one cooldown"));
        assert!(state
            .api_service
            .model_cooldown_active("a", "gpt-two")
            .expect("gpt-two cooldown"));
    }

    #[tokio::test]
    async fn revoked_key_is_not_retried_after_auth_failure() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = spawn_mock_server(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"invalid api key"}}"#,
            Some(hits.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &url)]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let first = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("first request");
        assert_eq!(first.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let config = store.load().expect("config");
        assert_eq!(
            config.health.get("a").expect("health").status,
            HealthStatusKind::AuthFailed
        );

        // key 已被判定吊销：即使没有备选账号也不能再拿它去上游循环重试。
        let second = proxy_inner(
            state,
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("second request");
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(second.into_body(), 1024).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["error"]["code"], "no_available_provider");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_load_is_tracked_until_response_body_is_consumed() {
        let url = spawn_mock_server(StatusCode::OK, "ok", None).await;
        let store = store_with_group(vec![provider("a", &url)]);
        let state = RelayState::new(store, reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");
        assert_eq!(response.status(), StatusCode::OK);
        // 响应头已返回但 body 还没消费：LeastLoaded 必须仍统计到这条在途请求。
        assert_eq!(state.provider_inflight_count("a"), 1);

        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"ok");
        assert_eq!(state.provider_inflight_count("a"), 0);
    }

    #[tokio::test]
    async fn session_affinity_survives_retry_budget_truncation() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(StatusCode::OK, "from a", Some(hits_a.clone())).await;
        let url_b = spawn_mock_server(StatusCode::OK, "from b", None).await;
        let url_c = spawn_mock_server(StatusCode::OK, "from c", Some(hits_c.clone())).await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let mut session_headers = HeaderMap::new();
        session_headers.insert("x-session-id", HeaderValue::from_static("thread-affinity"));

        // 先把会话绑定到 c：临时把 c 排到首位发起第一次请求。
        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["c".to_string(), "a".to_string(), "b".to_string()];
                Ok(())
            })
            .expect("reorder");
        let first = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("first request");
        assert_eq!(
            &to_bytes(first.into_body(), 1024).await.expect("first body")[..],
            b"from c"
        );

        // 恢复顺序并设 retry_budget=1(候选截断到 2 个)：绑定的 c 排在截断线
        // 之后，只有先做粘性提升再截断才能保住绑定。
        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["a".to_string(), "b".to_string(), "c".to_string()];
                config.relay.retry_budget = 1;
                Ok(())
            })
            .expect("tighten budget");
        let sticky = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers,
            Bytes::new(),
        )
        .await
        .expect("sticky request");
        assert_eq!(
            &to_bytes(sticky.into_body(), 1024)
                .await
                .expect("sticky body")[..],
            b"from c"
        );
        assert_eq!(hits_a.load(Ordering::SeqCst), 0);
        assert_eq!(hits_c.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn manual_priority_failback_rebinds_to_the_selected_higher_provider() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(StatusCode::OK, "from a", Some(hits_a.clone())).await;
        let url_b = spawn_mock_server(StatusCode::OK, "from b", Some(hits_b.clone())).await;
        let url_c = spawn_mock_server(StatusCode::OK, "from c", Some(hits_c.clone())).await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let mut session_headers = HeaderMap::new();
        session_headers.insert("x-session-id", HeaderValue::from_static("thread-failback"));

        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["c".to_string(), "a".to_string(), "b".to_string()];
                Ok(())
            })
            .expect("bind order");
        let bound = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("bound request");
        assert_eq!(
            &to_bytes(bound.into_body(), 1024).await.expect("bound body")[..],
            b"from c"
        );

        store
            .update(|config| {
                let group = config.groups.get_mut("test").expect("group");
                group.provider_order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
                group.priority_failback_revision += 1;
                group.priority_failback_target_provider_id = Some("a".to_string());
                config.health.insert(
                    "a".to_string(),
                    ProviderHealth {
                        status: HealthStatusKind::Cooldown,
                        cooldown_until: Some(Utc::now() + ChronoDuration::minutes(5)),
                        ..ProviderHealth::default()
                    },
                );
                config.relay.retry_budget = 1;
                Ok(())
            })
            .expect("request failback");
        let failback = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("failback request");
        assert_eq!(
            &to_bytes(failback.into_body(), 1024)
                .await
                .expect("failback body")[..],
            b"from a"
        );

        let sticky = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers,
            Bytes::new(),
        )
        .await
        .expect("sticky request");
        assert_eq!(
            &to_bytes(sticky.into_body(), 1024)
                .await
                .expect("sticky body")[..],
            b"from a"
        );
        assert_eq!(hits_a.load(Ordering::SeqCst), 2);
        assert_eq!(hits_b.load(Ordering::SeqCst), 0);
        assert_eq!(hits_c.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manual_priority_failback_still_prioritizes_target_when_current_is_unavailable() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(StatusCode::OK, "from a", Some(hits_a.clone())).await;
        let url_b = spawn_mock_server(StatusCode::OK, "from b", Some(hits_b.clone())).await;
        let url_c = spawn_mock_server(StatusCode::OK, "from c", Some(hits_c.clone())).await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let mut session_headers = HeaderMap::new();
        session_headers.insert(
            "x-session-id",
            HeaderValue::from_static("thread-unavailable-current"),
        );

        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["c".to_string(), "a".to_string(), "b".to_string()];
                Ok(())
            })
            .expect("bind order");
        let bound = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("bound request");
        assert_eq!(
            &to_bytes(bound.into_body(), 1024).await.expect("bound body")[..],
            b"from c"
        );

        store
            .update(|config| {
                let group = config.groups.get_mut("test").expect("group");
                group.provider_order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
                group.priority_failback_revision += 1;
                group.priority_failback_target_provider_id = Some("b".to_string());
                config.health.insert(
                    "c".to_string(),
                    ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        ..ProviderHealth::default()
                    },
                );
                config.relay.retry_budget = 1;
                Ok(())
            })
            .expect("request failback");
        let failback = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers,
            Bytes::new(),
        )
        .await
        .expect("failback request");
        assert_eq!(
            &to_bytes(failback.into_body(), 1024)
                .await
                .expect("failback body")[..],
            b"from b"
        );
        assert_eq!(hits_a.load(Ordering::SeqCst), 0);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert_eq!(hits_c.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_priority_failback_keeps_the_current_provider_inside_retry_budget() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(StatusCode::OK, "from a", Some(hits_a.clone())).await;
        let url_b = spawn_mock_server(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed b",
            Some(hits_b.clone()),
        )
        .await;
        let url_c = spawn_mock_server(StatusCode::OK, "from c", Some(hits_c.clone())).await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let mut session_headers = HeaderMap::new();
        session_headers.insert(
            "x-session-id",
            HeaderValue::from_static("thread-failed-failback"),
        );

        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").provider_order =
                    vec!["c".to_string(), "a".to_string(), "b".to_string()];
                Ok(())
            })
            .expect("bind order");
        let bound = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers.clone(),
            Bytes::new(),
        )
        .await
        .expect("bound request");
        assert_eq!(
            &to_bytes(bound.into_body(), 1024).await.expect("bound body")[..],
            b"from c"
        );

        store
            .update(|config| {
                let group = config.groups.get_mut("test").expect("group");
                group.provider_order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
                group.priority_failback_revision += 1;
                group.priority_failback_target_provider_id = Some("b".to_string());
                config.relay.retry_budget = 1;
                Ok(())
            })
            .expect("request failback");
        let failback = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            session_headers,
            Bytes::new(),
        )
        .await
        .expect("failback request");
        assert_eq!(
            &to_bytes(failback.into_body(), 1024)
                .await
                .expect("failback body")[..],
            b"from c"
        );
        assert_eq!(hits_a.load(Ordering::SeqCst), 0);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert_eq!(hits_c.load(Ordering::SeqCst), 2);
    }

    async fn spawn_mock_server(
        status: StatusCode,
        body: &'static str,
        hits: Option<Arc<AtomicUsize>>,
    ) -> String {
        let app = Router::new().route(
            "/{*path}",
            any(move || {
                let hits = hits.clone();
                async move {
                    if let Some(hits) = hits {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    async fn spawn_sse_mock_server(body: &'static str, hits: Option<Arc<AtomicUsize>>) -> String {
        let app = Router::new().route(
            "/{*path}",
            any(move || {
                let hits = hits.clone();
                async move {
                    if let Some(hits) = hits {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    ([(header::CONTENT_TYPE, "text/event-stream")], body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    fn store_with_group(providers: Vec<ProviderConfig>) -> ConfigStore {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.keep().join("config.json"));
        let provider_order = providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>();
        store
            .update(|config| {
                config.providers = providers
                    .into_iter()
                    .map(|provider| (provider.id.clone(), provider))
                    .collect();
                config.relay.active_group_id = "test".to_string();
                config.groups.insert(
                    "test".to_string(),
                    ProviderGroup {
                        id: "test".to_string(),
                        name: "Test".to_string(),
                        policy: GroupPolicy::PriorityFallback,
                        provider_order,
                        provider_weights: Default::default(),
                        fallback_enabled: true,
                        priority_failback_interval_seconds: 0,
                        priority_failback_revision: 0,
                        priority_failback_target_provider_id: None,
                    },
                );
                Ok(())
            })
            .expect("config");
        store
    }

    fn provider(id: &str, base_url: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        }
    }
}
