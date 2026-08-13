use crate::content_encoding::{
    decode_request_body, RequestBodyDecodeError, MAX_REQUEST_BODY_BYTES,
};
use crate::events::{append_event, record_health_success, update_health};
use crate::state::{apply_group_policy, AffinityBindContext, RelayState};
use crate::upstream::{
    send_upstream, stream_response, text_response, upstream_url, UpstreamRequest,
    UpstreamRequestError,
};
use crate::{RequestAttemptFinish, RequestAttemptStart, RequestLogFinish, RequestLogStart};
use axum::{
    body::Body,
    extract::{rejection::BytesRejection, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::{
    provider_endpoint_is_chat_completions, redact_sensitive_text, ApiClient, CompanionConfig,
    GroupPolicy, HealthFailureKind, HealthStatusKind, ProviderConfig, ProviderGroup,
};
use codex_companion_health::{
    classify_failure, cooldown_active, mark_failure, mark_model_failure,
    normalize_expired_cooldown, repair_legacy_auth_misclassification,
};
use codex_companion_provider::selected_providers_for_group;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const UPSTREAM_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(300);
const UPSTREAM_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(15);

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
    let session_id = request_session_id(&headers, &body);
    let request_metadata = request_metadata(&body);
    let requested_model = request_metadata.model.clone();
    let mut config = state
        .store
        .load()
        .map_err(|error| format!("failed to load config: {error}"))?;
    let _ = state
        .api_service
        .prune_request_logs(config.relay.request_log_retention_days);
    let root_probe = is_relay_root_probe(&method, &uri);
    let client = match authenticate_client(&state, &config, &headers, !root_probe) {
        Ok(client) => client,
        Err((status, message)) => {
            record_request_start(&state, request_id, &method, &uri, &request_metadata, None);
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
        &request_metadata,
        client.as_ref().map(|client| client.id.as_str()),
    );
    if root_probe {
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
    if normalize_health(&mut config) {
        let _ = state.store.update(|current| {
            normalize_health(current);
            Ok(())
        });
    }
    let group = config
        .groups
        .get(&config.relay.active_group_id)
        .cloned()
        .ok_or_else(|| format!("active group not found: {}", config.relay.active_group_id))?;
    let explicit_preferred_provider = session_id
        .as_deref()
        .and_then(|session_id| {
            state
                .api_service
                .session_provider_preference(session_id)
                .ok()
                .flatten()
        })
        .filter(|provider_id| {
            group
                .provider_order
                .iter()
                .any(|candidate| candidate == provider_id)
                && config
                    .providers
                    .get(provider_id.as_str())
                    .is_some_and(|provider| provider.enabled)
        });
    let mut selected = selected_providers_for_group(&config, &group)
        .into_iter()
        .filter(|provider| provider.enabled)
        .collect::<Vec<_>>();
    if let Some(preferred_provider) = explicit_preferred_provider
        .as_deref()
        .and_then(|provider_id| config.providers.get(provider_id))
        .filter(|provider| !selected.iter().any(|candidate| candidate.id == provider.id))
    {
        selected.push(preferred_provider.clone());
    }
    if method == Method::GET && uri.path() == "/v1/models" {
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
    let priority_failback_claim = explicit_preferred_provider
        .is_none()
        .then(|| {
            affinity_preference
                .as_ref()
                .and_then(|(key, _)| state.claim_priority_failback_probe(key, &group))
        })
        .flatten();
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
    // 限重试。临时冷却(429/5xx/网络故障)排在健康账号之后，但仍保留完整
    // 后备链，避免所有账号短暂冷却时只尝试一个账号就结束对话。
    let has_alternatives = group.fallback_enabled && selected.len() > 1;
    let mut cooldown_probes = Vec::new();
    let mut candidates = selected
        .into_iter()
        .filter_map(|provider| {
            let health = config.health.get(&provider.id);
            if health.is_some_and(|health| matches!(health.status, HealthStatusKind::AuthFailed)) {
                return None;
            }
            if manually_requested_provider
                .as_deref()
                .is_some_and(|provider_id| provider_id == provider.id)
            {
                return Some(provider);
            }
            if !has_alternatives {
                return Some(provider);
            }
            let globally_available = health.is_none_or(|health| !cooldown_active(health));
            let model_available = requested_model.as_deref().is_none_or(|model| {
                !state
                    .api_service
                    .model_cooldown_active(&provider.id, model)
                    .unwrap_or(false)
            });
            if globally_available && model_available {
                return Some(provider);
            }
            let transient_failure = health
                .and_then(|health| health.last_failure_kind.as_ref())
                .is_none_or(|kind| {
                    matches!(
                        kind,
                        HealthFailureKind::RateLimited
                            | HealthFailureKind::UpstreamFailed
                            | HealthFailureKind::NetworkFailed
                            | HealthFailureKind::RequestRejected
                            | HealthFailureKind::Unknown
                    )
                });
            if transient_failure {
                cooldown_probes.push(provider);
            }
            None
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
    if let Some(preferred_provider) = explicit_preferred_provider.as_deref() {
        prioritize_session_affinity(&mut candidates, preferred_provider, None);
    } else if let Some((_, preference)) = affinity_preference.as_ref() {
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
    if group.fallback_enabled {
        cooldown_probes.sort_by_key(|provider| {
            (
                state.provider_inflight_count(&provider.id),
                config
                    .health
                    .get(&provider.id)
                    .and_then(|health| health.last_checked),
            )
        });
        candidates.extend(cooldown_probes);
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
        let attempt = (index + 1) as u16;
        let attempt_started_at = Instant::now();
        let route_reason = request_attempt_route_reason(
            index,
            &provider.id,
            explicit_preferred_provider.as_deref(),
            affinity_preference
                .as_ref()
                .map(|(_, preference)| preference.provider_id.as_str()),
            priority_probe_provider.as_deref(),
            priority_failback_claim.is_some_and(|claim| claim.manual),
        );
        record_request_attempt_start(&state, request_id, attempt, &provider.id, route_reason);
        if compact_request && provider_endpoint_is_chat_completions(&provider.base_url) {
            let message = format!(
                "Provider {} 仅支持 Chat Completions，无法处理 Responses Compact API",
                provider.name
            );
            last_error = Some(message.clone());
            record_request_attempt_finish(
                &state,
                request_id,
                attempt,
                Some(StatusCode::NOT_IMPLEMENTED),
                "failed",
                attempt_started_at,
                Some(&message),
            );
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
        let upstream_result = tokio::time::timeout(
            UPSTREAM_RESPONSE_HEADER_TIMEOUT,
            send_upstream(
                &state.client,
                &state.api_service,
                UpstreamRequest::new(&provider, &method, &uri, &headers, body.clone(), &upstream),
            ),
        )
        .await
        .unwrap_or_else(|_| {
            Err(UpstreamRequestError::from(format!(
                "上游网络等待响应超时（Provider {}）",
                provider.name
            )))
        });
        match upstream_result {
            Ok(mut response) if response.status().is_success() => {
                let upstream_status = response.status();
                let preflight = response
                    .preflight_stream_failure(group.fallback_enabled && index + 1 < candidate_count)
                    .await;
                if let Err(error) = preflight {
                    let failure = classify_failure(None, &error.classification_text());
                    let message = error.to_string();
                    record_provider_failure(
                        &state,
                        &config,
                        &provider.id,
                        requested_model.as_deref(),
                        &failure,
                        &message,
                    );
                    last_error = Some(message.clone());
                    record_request_attempt_finish(
                        &state,
                        request_id,
                        attempt,
                        Some(StatusCode::BAD_GATEWAY),
                        "failed",
                        attempt_started_at,
                        Some(&message),
                    );
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
                    if explicit_preferred_provider.is_none() {
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
                    record_request_attempt_finish(
                        &state,
                        request_id,
                        attempt,
                        Some(downstream_status),
                        "succeeded",
                        attempt_started_at,
                        None,
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
                record_request_attempt_finish(
                    &state,
                    request_id,
                    attempt,
                    Some(downstream_status),
                    "failed",
                    attempt_started_at,
                    Some(&message),
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
                let oauth_refresh_error = response.oauth_refresh_error().map(str::to_string);
                let oauth_refresh_failure = response.oauth_refresh_failure().cloned();
                let body_text = match tokio::time::timeout(
                    UPSTREAM_ERROR_BODY_TIMEOUT,
                    response.text(),
                )
                .await
                {
                    Ok(Ok(body)) => body,
                    Ok(Err(error)) => format!("读取上游错误响应失败: {error}"),
                    Err(_) => "读取上游错误响应超时".to_string(),
                };
                let failure = oauth_refresh_failure
                    .unwrap_or_else(|| classify_failure(Some(status.as_u16()), &body_text));
                let upstream_payload_too_large = status == StatusCode::PAYLOAD_TOO_LARGE;
                let request_incompatible = status == StatusCode::BAD_REQUEST;
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
                } else if let Some(refresh_error) = oauth_refresh_error {
                    format!(
                        "上游返回 {}；{}：{}",
                        status,
                        refresh_error,
                        compact_error_body(&body_text)
                    )
                } else {
                    format!("上游返回 {}: {}", status, compact_error_body(&body_text))
                };
                if !upstream_payload_too_large && !request_incompatible && !compact_unsupported {
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
                record_request_attempt_finish(
                    &state,
                    request_id,
                    attempt,
                    Some(status),
                    "failed",
                    attempt_started_at,
                    Some(&message),
                );
                // 上游在尚未提交任何响应前返回了明确的 HTTP 失败。无论本地能否
                // 精确识别其语义，都应给组内下一个 provider 一个机会；否则诸如
                // 404/405/422 的兼容层差异会直接终止 Codex 对话。
                let can_retry = index + 1 < candidate_count && group.fallback_enabled;
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
                    } else if request_incompatible {
                        "upstream_request_incompatible"
                    } else if compact_unsupported {
                        "responses_compact_unsupported"
                    } else {
                        failure_error_code(&failure.kind)
                    },
                    &message,
                ));
            }
            Err(error) => {
                let failure = error
                    .failure()
                    .cloned()
                    .unwrap_or_else(|| classify_failure(None, error.message_text()));
                let message = compact_error_body(error.message_text());
                record_provider_failure(
                    &state,
                    &config,
                    &provider.id,
                    requested_model.as_deref(),
                    &failure,
                    &message,
                );
                last_error = Some(message.clone());
                record_request_attempt_finish(
                    &state,
                    request_id,
                    attempt,
                    None,
                    "failed",
                    attempt_started_at,
                    Some(&message),
                );
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
        HealthFailureKind::RequestRejected => "upstream_request_rejected",
        HealthFailureKind::NetworkFailed => "upstream_network_error",
        HealthFailureKind::UpstreamFailed => "upstream_error",
        HealthFailureKind::Unknown => "upstream_unknown_error",
    }
}

fn authenticate_client(
    state: &RelayState,
    config: &CompanionConfig,
    headers: &HeaderMap,
    enforce_config_key: bool,
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
    if client.is_none()
        && (state.enforce_api_key
            || (enforce_config_key && config.relay.require_api_key)
            || browser_origin)
    {
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

#[derive(Debug, Default, PartialEq, Eq)]
struct RequestMetadata {
    model: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

fn request_metadata(body: &[u8]) -> RequestMetadata {
    const MAX_METADATA_CHARS: usize = 160;

    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return RequestMetadata::default();
    };
    RequestMetadata {
        model: bounded_request_string(value.get("model"), MAX_METADATA_CHARS),
        reasoning_effort: bounded_request_string(
            value
                .pointer("/reasoning/effort")
                .or_else(|| value.get("reasoning_effort")),
            MAX_METADATA_CHARS,
        ),
        service_tier: bounded_request_string(value.get("service_tier"), MAX_METADATA_CHARS),
    }
}

fn bounded_request_string(value: Option<&Value>, max_chars: usize) -> Option<String> {
    let value = value?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(max_chars).collect())
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
    if matches!(&failure.kind, HealthFailureKind::RequestRejected) {
        return;
    }
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
    metadata: &RequestMetadata,
    client_id: Option<&str>,
) {
    let path = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    let _ = state.api_service.record_request_start(RequestLogStart {
        request_id,
        method: method.as_str(),
        path,
        model: metadata.model.as_deref(),
        reasoning_effort: metadata.reasoning_effort.as_deref(),
        service_tier: metadata.service_tier.as_deref(),
        client_id,
    });
}

fn request_attempt_route_reason(
    index: usize,
    provider_id: &str,
    explicit_preferred_provider_id: Option<&str>,
    affinity_provider_id: Option<&str>,
    priority_probe_provider_id: Option<&str>,
    manual_priority_probe: bool,
) -> &'static str {
    if index > 0 {
        return "fallback";
    }
    if priority_probe_provider_id == Some(provider_id) {
        return if manual_priority_probe {
            "manual_failback"
        } else {
            "automatic_failback"
        };
    }
    if explicit_preferred_provider_id == Some(provider_id) {
        return "session_preference";
    }
    if affinity_provider_id == Some(provider_id) {
        return "affinity";
    }
    "policy"
}

fn record_request_attempt_start(
    state: &RelayState,
    request_id: &str,
    attempt: u16,
    provider_id: &str,
    route_reason: &str,
) {
    let _ = state
        .api_service
        .record_request_attempt_start(RequestAttemptStart {
            request_id,
            attempt,
            provider_id,
            route_reason,
        });
}

fn record_request_attempt_finish(
    state: &RelayState,
    request_id: &str,
    attempt: u16,
    status: Option<StatusCode>,
    outcome: &str,
    started_at: Instant,
    error: Option<&str>,
) {
    let _ = state
        .api_service
        .record_request_attempt_finish(RequestAttemptFinish {
            request_id,
            attempt,
            status_code: status.map(|status| status.as_u16()),
            outcome,
            latency_ms: started_at
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            error,
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

fn request_session_id(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    for header in ["session_id", "x-session-id", "x-amp-thread-id"] {
        if let Some(value) = headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }

    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        for path in [
            &["metadata", "session_id"][..],
            &["conversation_id"][..],
            &["thread_id"][..],
            &["prompt_cache_key"][..],
        ] {
            if let Some(value) = json_string_at_path(&value, path)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(value.to_string());
            }
        }
    }
    headers
        .get("x-client-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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

fn normalize_health(config: &mut CompanionConfig) -> bool {
    let mut repaired = false;
    for health in config.health.values_mut() {
        repaired |= repair_legacy_auth_misclassification(health);
        normalize_expired_cooldown(health);
    }
    repaired
}

fn compact_error_body(body: &str) -> String {
    let redacted = redact_sensitive_text(body);
    let text = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
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
    use crate::upstream::MAX_SSE_FRAME_BYTES;
    use axum::{body::to_bytes, routing::any, Router};
    use chrono::{Duration as ChronoDuration, Utc};
    use codex_companion_core::{
        ApiClientCreate, ConfigStore, GroupPolicy, HealthFailureKind, ProviderConfig,
        ProviderGroup, ProviderHealth, ProviderKind,
    };
    use std::collections::BTreeMap;
    use std::convert::Infallible;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn request_metadata_reads_observability_fields() {
        assert_eq!(
            request_metadata(
                br#"{"model":"gpt-5.6-sol","reasoning":{"effort":"high"},"service_tier":"priority"}"#,
            ),
            RequestMetadata {
                model: Some("gpt-5.6-sol".to_string()),
                reasoning_effort: Some("high".to_string()),
                service_tier: Some("priority".to_string()),
            }
        );
        assert_eq!(
            request_metadata(br#"{"reasoning_effort":"xhigh"}"#).reasoning_effort,
            Some("xhigh".to_string())
        );
    }

    #[test]
    fn relay_root_probe_is_handled_locally() {
        let root: Uri = "/v1".parse().expect("uri");
        let models: Uri = "/v1/models".parse().expect("uri");

        assert!(is_relay_root_probe(&Method::GET, &root));
        assert!(!is_relay_root_probe(&Method::POST, &root));
        assert!(!is_relay_root_probe(&Method::GET, &models));
    }

    #[tokio::test]
    async fn browser_root_probe_requires_a_client_key_but_local_self_test_stays_public() {
        let store = store_with_group(Vec::new());
        store
            .update(|config| {
                config.relay.require_api_key = true;
                Ok(())
            })
            .expect("strict mode");
        let state = RelayState::new(store, reqwest::Client::new());

        let local_response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("local root probe");
        assert_eq!(local_response.status(), StatusCode::OK);

        let mut browser_headers = HeaderMap::new();
        browser_headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:1420"),
        );
        let browser_response = proxy_inner(
            state,
            Method::GET,
            "/v1".parse().expect("uri"),
            browser_headers,
            Bytes::new(),
        )
        .await
        .expect("browser root probe");
        assert_eq!(browser_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn declared_provider_models_are_served_from_the_local_catalog() {
        let store = store_with_group(vec![provider(
            "official",
            "https://chatgpt.com/backend-api/codex",
        )]);
        store
            .update(|config| {
                let provider = config.providers.get_mut("official").expect("provider");
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
        let request = state
            .api_service
            .snapshot(10)
            .expect("request snapshot")
            .recent_requests
            .into_iter()
            .next()
            .expect("request log");
        assert_eq!(request.attempt_log.len(), 2);
        assert_eq!(request.attempt_log[0].provider_id, "a");
        assert_eq!(request.attempt_log[0].outcome, "failed");
        assert!(request.attempt_log[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("500")));
        assert_eq!(request.attempt_log[1].provider_id, "b");
        assert_eq!(request.attempt_log[1].route_reason, "fallback");
        assert_eq!(request.attempt_log[1].outcome, "succeeded");
    }

    #[tokio::test]
    async fn falls_back_after_upstream_connection_error_before_output() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            None,
        )
        .await;
        let store = store_with_group(vec![
            provider("offline", &format!("http://{address}")),
            provider("healthy", &provider_b_url),
        ]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
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
            Some("healthy")
        );
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("offline")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::NetworkFailed)
        );
    }

    #[tokio::test]
    async fn falls_back_when_provider_rejects_the_request_as_invalid() {
        let incompatible_hits = Arc::new(AtomicUsize::new(0));
        let compatible_hits = Arc::new(AtomicUsize::new(0));
        let incompatible_url = spawn_mock_server(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Upstream rejected the request as invalid","type":"invalid_request_error"}}"#,
            Some(incompatible_hits.clone()),
        )
        .await;
        let compatible_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(compatible_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("incompatible", &incompatible_url),
            provider("compatible", &compatible_url),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"gpt-test","input":[{"type":"custom_tool_call","id":"item_99fb83474df510b04e475dc5","call_id":"call_1","name":"exec","input":""}]}"#,
            ),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("compatible")
        );
        assert_eq!(incompatible_hits.load(Ordering::SeqCst), 1);
        assert_eq!(compatible_hits.load(Ordering::SeqCst), 1);
        assert!(!store
            .load()
            .expect("config")
            .health
            .contains_key("incompatible"));
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("Upstream rejected the request as invalid"));
        assert!(events.contains("\"kind\":\"fallback\""));
        let request = state
            .api_service
            .snapshot(10)
            .expect("request snapshot")
            .recent_requests
            .into_iter()
            .next()
            .expect("request log");
        assert_eq!(request.attempt_log.len(), 2);
        assert_eq!(request.attempt_log[0].provider_id, "incompatible");
        assert_eq!(request.attempt_log[0].status_code, Some(400));
        assert_eq!(request.attempt_log[1].provider_id, "compatible");
        assert_eq!(request.attempt_log[1].route_reason, "fallback");
        assert_eq!(request.attempt_log[1].outcome, "succeeded");
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
    async fn falls_back_when_responses_200_body_is_empty_plaintext_or_invalid() {
        for invalid_body in [
            "",
            "upstream accepted the request",
            r#"{"id":"resp_a","status":"completed","output":[]}"#,
        ] {
            let provider_a_hits = Arc::new(AtomicUsize::new(0));
            let provider_b_hits = Arc::new(AtomicUsize::new(0));
            let provider_a_url =
                spawn_mock_server(StatusCode::OK, invalid_body, Some(provider_a_hits.clone()))
                    .await;
            let provider_b_url = spawn_mock_server(
                StatusCode::OK,
                r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
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
            assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
            assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn falls_back_when_chat_200_response_lacks_assistant_message() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"chatcmpl_a","choices":[{"finish_reason":"stop"}]}"#,
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

        let response = proxy_inner(
            RelayState::new(store, reqwest::Client::new()),
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
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn falls_back_for_compatibility_statuses_before_any_output() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            let provider_a_hits = Arc::new(AtomicUsize::new(0));
            let provider_b_hits = Arc::new(AtomicUsize::new(0));
            let provider_a_url = spawn_mock_server(
                status,
                r#"{"error":{"message":"provider compatibility mismatch"}}"#,
                Some(provider_a_hits.clone()),
            )
            .await;
            let provider_b_url = spawn_mock_server(
                StatusCode::OK,
                r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
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
                Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
            )
            .await
            .expect("proxy");

            assert_eq!(response.status(), StatusCode::OK, "status {status}");
            assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1, "status {status}");
            assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1, "status {status}");
        }
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
    async fn falls_back_when_successful_response_cannot_be_converted() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_sse_mock_server(
            "data: {\"type\":\"response.completed\"}\n\n",
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let auth_path = store.data_dir().join("official-a-auth.json");
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"opaque-valid-token"}}"#,
        )
        .expect("write auth");
        store
            .update(|config| {
                let provider = config.providers.get_mut("a").expect("provider a");
                provider.kind = ProviderKind::OfficialCodex;
                provider.auth_ref = Some(format!("file:{}", auth_path.display()));
                Ok(())
            })
            .expect("configure official provider");
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state.clone(),
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
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        let request = state
            .api_service
            .snapshot(10)
            .expect("request snapshot")
            .recent_requests
            .into_iter()
            .next()
            .expect("request log");
        assert_eq!(request.attempt_log.len(), 2);
        assert_eq!(request.attempt_log[0].provider_id, "a");
        assert_eq!(request.attempt_log[0].outcome, "failed");
        assert_eq!(request.attempt_log[1].provider_id, "b");
        assert_eq!(request.attempt_log[1].route_reason, "fallback");
        assert_eq!(request.attempt_log[1].outcome, "succeeded");
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("无法转换为本地协议"));
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn falls_back_when_sse_only_returns_done_without_a_response() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url =
            spawn_sse_mock_server("data: [DONE]\n\n", Some(provider_a_hits.clone())).await;
        let provider_b_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok from b\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
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
    async fn preserves_large_split_sse_frame_without_failing_over() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let first = Bytes::from(format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{}",
            "x".repeat(16 * 1024)
        ));
        let provider_a_url = spawn_delayed_sse_mock_server(
            first,
            Bytes::from_static(
                b"\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            ),
            Duration::from_millis(20),
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"fallback\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
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
            Some("a")
        );
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        assert!(body.len() > 16 * 1024);
        assert!(String::from_utf8_lossy(&body).contains("response.output_text.delta"));
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn falls_back_when_sse_frame_reaches_the_eight_mib_limit() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let mut frame = Vec::with_capacity(MAX_SSE_FRAME_BYTES);
        frame.extend_from_slice(b"data: ");
        frame.resize(MAX_SSE_FRAME_BYTES, b'x');
        let provider_a_url = spawn_delayed_sse_mock_server(
            Bytes::from(frame),
            Bytes::from_static(b"\n\n"),
            Duration::from_millis(20),
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

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("b")
        );
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn falls_back_for_large_split_sse_failure() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let first = Bytes::from(format!(
            concat!(
                "data: {{\"type\":\"response.failed\",\"response\":{{\"status\":\"failed\",",
                "\"error\":{{\"message\":\"exceeded retry limit, last status: 429 Too Many Requests {}"
            ),
            "x".repeat(16 * 1024)
        ));
        let provider_a_url = spawn_delayed_sse_mock_server(
            first,
            Bytes::from_static(b"\"}}}\n\n"),
            Duration::from_millis(20),
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok from b\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
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
    async fn accepts_cr_only_responses_sse_without_failing_over() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let expected = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cr\"}}\r\r",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cr\",",
            "\"status\":\"completed\"}}\r\r"
        );
        let provider_a_url = spawn_sse_mock_server(expected, Some(provider_a_hits.clone())).await;
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

        assert_eq!(response.status(), StatusCode::OK);
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
        assert_eq!(body.as_ref(), expected.as_bytes());
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn falls_back_for_cr_only_responses_sse_failure() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",",
                "\"error\":{\"message\":\"429 Too Many Requests\"}}}\r\r"
            ),
            Some(provider_a_hits.clone()),
        )
        .await;
        let provider_b_url = spawn_sse_mock_server(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok from b\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
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
    async fn preserves_crlf_sse_boundary_split_across_transport_chunks() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let first = Bytes::from_static(
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\r",
        );
        let second = Bytes::from_static(b"\n\r\n");
        let provider_a_url = spawn_delayed_sse_mock_server(
            first.clone(),
            second.clone(),
            Duration::from_millis(20),
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

        assert_eq!(response.status(), StatusCode::OK);
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
        let mut expected = first.to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(body.as_ref(), expected);
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn falls_back_from_opaque_event_stream_when_group_has_an_alternative() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let first = Bytes::from_static(b"opaque event-stream ");
        let second = Bytes::from_static(b"payload\0with binary tail");
        let provider_a_url = spawn_delayed_sse_mock_server(
            first.clone(),
            second.clone(),
            Duration::from_millis(20),
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
        let state = RelayState::new(store.clone(), reqwest::Client::new());

        let response = proxy_inner(
            state,
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
        assert!(String::from_utf8_lossy(&body).contains("response.completed"));
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("a")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::UpstreamFailed)
        );
    }

    #[tokio::test]
    async fn single_provider_passes_through_opaque_event_stream_byte_for_byte() {
        let provider_hits = Arc::new(AtomicUsize::new(0));
        let first = Bytes::from_static(b"opaque event-stream ");
        let second = Bytes::from_static(b"payload\0with binary tail");
        let provider_url = spawn_delayed_sse_mock_server(
            first.clone(),
            second.clone(),
            Duration::from_millis(20),
            Some(provider_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &provider_url)]);

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
            Some("a")
        );
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("body");
        let mut expected = first.to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(body.as_ref(), expected);
        assert_eq!(provider_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn falls_back_from_mislabeled_json_stream_error_with_semantic_health() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_delayed_sse_mock_server(
            Bytes::from_static(br#"{"error":{"message":"429 Too"#),
            Bytes::from_static(br#" Many Requests"}}"#),
            Duration::from_millis(20),
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
            RelayState::new(store.clone(), reqwest::Client::new()),
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
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("a")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::RateLimited)
        );
    }

    #[tokio::test]
    async fn falls_back_when_sse_rate_limit_happens_after_reasoning_preamble() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_delayed_sse_mock_server(
            Bytes::from_static(
                concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_a\"}}\n\n",
                    "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\"}}\n\n",
                    "data: {\"type\":\"response.reasoning_summary_part.added\",\"part\":{\"type\":\"summary_text\"}}\n\n",
                    "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checking capacity\"}\n\n"
                )
                .as_bytes(),
            ),
            Bytes::from_static(
                concat!(
                    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\"}}\n\n",
                    "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",",
                    "\"error\":{\"message\":\"exceeded retry limit, last status: 429 Too Many Requests\"}}}\n\n"
                )
                .as_bytes(),
            ),
            Duration::from_millis(20),
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
        assert!(state
            .api_service
            .model_cooldown_active("a", "gpt-test")
            .expect("rate limit cooldown"));
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("a")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::RateLimited)
        );
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
                "event: message\n",
                "data: {\"id\":\"chatcmpl_sse\",\"model\":\"gpt-test\",",
                "\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\n",
                "event: done\n",
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
        assert_eq!(
            request_session_id(
                &HeaderMap::new(),
                br#"{"metadata":{"session_id":"session-123"}}"#,
            )
            .as_deref(),
            Some("session-123")
        );
        let mut request_headers = HeaderMap::new();
        request_headers.insert(
            "x-client-request-id",
            HeaderValue::from_static("request-456"),
        );
        assert_eq!(
            request_session_id(
                &request_headers,
                br#"{"metadata":{"session_id":"session-123"}}"#,
            )
            .as_deref(),
            Some("session-123")
        );
        assert_eq!(
            request_session_id(&request_headers, b"").as_deref(),
            Some("request-456")
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
    async fn explicit_session_preference_overrides_policy_order() {
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
        store
            .update(|config| {
                config.groups.get_mut("test").expect("group").policy = GroupPolicy::Manual;
                Ok(())
            })
            .expect("manual policy");
        let state = RelayState::new(store, reqwest::Client::new());
        state
            .api_service
            .set_session_provider_preference("thread-preferred", "b")
            .expect("preference");
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("thread-preferred"));

        let response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            headers.clone(),
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
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 0);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        let requests = state.api_service.list_requests(1).expect("request log");
        assert_eq!(
            requests[0].attempt_log[0].route_reason,
            "session_preference"
        );
    }

    #[tokio::test]
    async fn explicit_session_preference_falls_back_without_being_overwritten() {
        let provider_a_hits = Arc::new(AtomicUsize::new(0));
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url =
            spawn_mock_server(StatusCode::OK, "from a", Some(provider_a_hits.clone())).await;
        let provider_b_url = spawn_mock_server(
            StatusCode::SERVICE_UNAVAILABLE,
            "b unavailable",
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState::new(store, reqwest::Client::new());
        state
            .api_service
            .set_session_provider_preference("thread-fallback", "b")
            .expect("preference");
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("thread-fallback"));

        let response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            headers.clone(),
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
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            state
                .api_service
                .session_provider_preference("thread-fallback")
                .expect("stored preference")
                .as_deref(),
            Some("b")
        );
        let requests = state.api_service.list_requests(1).expect("request log");
        assert_eq!(
            requests[0].attempt_log[0].route_reason,
            "session_preference"
        );
        assert_eq!(requests[0].attempt_log[1].route_reason, "fallback");

        let second_response = proxy_inner(
            state.clone(),
            Method::GET,
            "/v1/models".parse().expect("uri"),
            headers,
            Bytes::new(),
        )
        .await
        .expect("second proxy");

        assert_eq!(second_response.status(), StatusCode::OK);
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider_a_hits.load(Ordering::SeqCst), 2);
        assert_eq!(
            state
                .api_service
                .session_provider_preference("thread-fallback")
                .expect("stored preference")
                .as_deref(),
            Some("b")
        );
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
        let provider_url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_a","object":"response","status":"completed","output":[]}"#,
            Some(hits.clone()),
        )
        .await;
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

    #[test]
    fn browser_origin_requires_a_valid_client_key_even_in_local_mode() {
        let store = store_with_group(Vec::new());
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let config = store.load().expect("config");
        assert!(!config.relay.require_api_key);

        let mut browser_headers = HeaderMap::new();
        browser_headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:1420"),
        );
        let missing = authenticate_client(&state, &config, &browser_headers, false)
            .expect_err("browser request without a key");
        assert_eq!(missing.0, StatusCode::UNAUTHORIZED);

        let secret = state
            .api_service
            .create_client(ApiClientCreate {
                name: "browser client".to_string(),
                allowed_models: Vec::new(),
            })
            .expect("client");
        browser_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", secret.api_key)).expect("auth header"),
        );
        let authenticated = authenticate_client(&state, &config, &browser_headers, false)
            .expect("authenticated browser request")
            .expect("client");
        assert_eq!(authenticated.id, secret.client.id);
    }

    #[test]
    fn non_loopback_runtime_auth_floor_survives_config_changes() {
        let store = store_with_group(Vec::new());
        let state = RelayState::new_with_api_key_floor(store.clone(), reqwest::Client::new(), true);
        let config = store.load().expect("config");
        assert!(!config.relay.require_api_key);

        let missing = authenticate_client(&state, &config, &HeaderMap::new(), true)
            .expect_err("runtime floor must require a key");
        assert_eq!(missing.0, StatusCode::UNAUTHORIZED);
        let root_probe = authenticate_client(&state, &config, &HeaderMap::new(), false)
            .expect_err("non-loopback root probe must require a key");
        assert_eq!(root_probe.0, StatusCode::UNAUTHORIZED);
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
        let url_b = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(hits_b.clone()),
        )
        .await;
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
    async fn insufficient_balance_403_falls_back_as_quota_exhaustion() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::FORBIDDEN,
            r#"{"error":{"code":"INSUFFICIENT_BALANCE","message":"Insufficient account balance"}}"#,
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(hits_b.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &url_a), provider("b", &url_b)]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        let health = store.load().expect("config").health;
        assert_eq!(
            health.get("a").map(|health| health.status.clone()),
            Some(HealthStatusKind::QuotaExhausted)
        );
        assert_eq!(
            health
                .get("a")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::QuotaExhausted)
        );
    }

    #[tokio::test]
    async fn content_policy_403_falls_back_without_poisoning_provider_health() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::FORBIDDEN,
            r#"{"error":{"code":"content_policy_violation","message":"request rejected by content policy"}}"#,
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(hits_b.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &url_a), provider("b", &url_b)]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert!(!store.load().expect("config").health.contains_key("a"));
    }

    #[tokio::test]
    async fn legacy_content_policy_auth_failure_is_repaired_before_routing() {
        let hits = Arc::new(AtomicUsize::new(0));
        let url = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_a","object":"response","status":"completed","output":[]}"#,
            Some(hits.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &url)]);
        store
            .update(|config| {
                config.health.insert(
                    "a".to_string(),
                    ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        last_error: Some(
                            "上游返回 403 Forbidden: content_policy_violation".to_string(),
                        ),
                        last_failure_kind: Some(HealthFailureKind::AuthFailed),
                        failure_count: 1,
                        ..ProviderHealth::default()
                    },
                );
                Ok(())
            })
            .expect("seed legacy health");

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("a")
                .map(|health| health.status.clone()),
            Some(HealthStatusKind::Healthy)
        );
    }

    #[tokio::test]
    async fn usage_limit_429_falls_back_to_the_next_provider() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_in_seconds":447522}}"#,
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(hits_b.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &url_a), provider("b", &url_b)]);

        let response = proxy_inner(
            RelayState::new(store.clone(), reqwest::Client::new()),
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("response json");
        assert_eq!(value["status"], "completed");
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("a")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(HealthFailureKind::QuotaExhausted)
        );
    }

    #[tokio::test]
    async fn cooled_providers_fallback_within_the_same_request() {
        let hits_a = Arc::new(AtomicUsize::new(0));
        let hits_b = Arc::new(AtomicUsize::new(0));
        let hits_c = Arc::new(AtomicUsize::new(0));
        let url_a = spawn_mock_server(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"code":"rate_limit_exceeded","message":"retry later"}}"#,
            Some(hits_a.clone()),
        )
        .await;
        let url_b = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_b","object":"response","status":"completed","output":[]}"#,
            Some(hits_b.clone()),
        )
        .await;
        let url_c = spawn_mock_server(
            StatusCode::OK,
            r#"{"id":"resp_c","object":"response","status":"completed","output":[]}"#,
            Some(hits_c.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &url_a),
            provider("b", &url_b),
            provider("c", &url_c),
        ]);
        let state = RelayState::new(store.clone(), reqwest::Client::new());
        let failure = classify_failure(Some(429), "rate_limit_exceeded");
        let now = Utc::now();
        store
            .update(|config| {
                for (provider_id, age_seconds) in [("a", 30), ("b", 20), ("c", 10)] {
                    let health = config
                        .health
                        .entry(provider_id.to_string())
                        .or_insert_with(ProviderHealth::default);
                    mark_model_failure(health, &failure, "seed rate limit".to_string());
                    health.last_checked = Some(now - ChronoDuration::seconds(age_seconds));
                }
                Ok(())
            })
            .expect("seed health cooldowns");
        for provider_id in ["a", "b", "c"] {
            state
                .api_service
                .set_model_cooldown(provider_id, "gpt-test", "seed rate limit", 300)
                .expect("seed model cooldown");
        }

        let response = proxy_inner(
            state,
            Method::POST,
            "/v1/responses".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
        )
        .await
        .expect("fallback request");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        let value: Value = serde_json::from_slice(&body).expect("response json");
        assert_eq!(value["status"], "completed");
        assert_eq!(hits_a.load(Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(Ordering::SeqCst), 1);
        assert_eq!(hits_c.load(Ordering::SeqCst), 0);
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

    async fn spawn_delayed_sse_mock_server(
        first: Bytes,
        second: Bytes,
        delay: Duration,
        hits: Option<Arc<AtomicUsize>>,
    ) -> String {
        let app = Router::new().route(
            "/{*path}",
            any(move || {
                let hits = hits.clone();
                let first = first.clone();
                let second = second.clone();
                async move {
                    if let Some(hits) = hits {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    let stream = futures_util::stream::unfold(0_u8, move |step| {
                        let first = first.clone();
                        let second = second.clone();
                        async move {
                            match step {
                                0 => Some((Ok::<Bytes, Infallible>(first), 1)),
                                1 => {
                                    tokio::time::sleep(delay).await;
                                    Some((Ok::<Bytes, Infallible>(second), 2))
                                }
                                _ => None,
                            }
                        }
                    });
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(stream))
                        .expect("SSE response")
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
