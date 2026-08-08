use crate::events::{append_event, record_health_success, update_health};
use crate::state::{apply_group_policy, RelayState};
use crate::upstream::{
    normalize_official_input_item_ids, response_event_has_visible_output, semantic_failure_message,
};
use axum::{
    extract::{ws::Message as ClientMessage, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use codex_companion_core::{
    ApiClient, HealthFailureKind, HealthStatusKind, ProviderConfig, ProviderKind,
};
use codex_companion_health::{
    classify_failure, cooldown_active, mark_failure, normalize_expired_cooldown,
    repair_legacy_auth_misclassification,
};
use codex_companion_provider::{
    ensure_agent_identity_authorization, ensure_codex_auth_snapshot, provider_uses_agent_identity,
    resolve_auth_token, selected_providers_for_group,
};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as UpstreamMessage},
    MaybeTlsStream, WebSocketStream,
};

const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_PREFLIGHT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const WEBSOCKET_PREFLIGHT_MAX_DURATION: Duration = Duration::from_secs(120);
const WEBSOCKET_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_WEBSOCKET_PREFLIGHT_MESSAGES: usize = 128;
const MAX_WEBSOCKET_PREFLIGHT_BYTES: usize = 1024 * 1024;

type UpstreamWebSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type UpstreamSink = SplitSink<UpstreamWebSocket, UpstreamMessage>;
type UpstreamStream = SplitStream<UpstreamWebSocket>;
type ClientWebSocket = axum::extract::ws::WebSocket;
type ClientSink = SplitSink<ClientWebSocket, ClientMessage>;

pub(crate) async fn responses_websocket(
    State(state): State<RelayState>,
    websocket: WebSocketUpgrade,
    headers: HeaderMap,
) -> Response {
    let api_client = match authenticate_websocket_client(&state, &headers) {
        Ok(api_client) => api_client,
        Err(error) => return error.into_response(),
    };
    let allowed_models = api_client
        .map(|api_client| api_client.allowed_models)
        .unwrap_or_default();
    let preferred_provider = websocket_session_id(&headers).and_then(|session_id| {
        state
            .api_service
            .session_provider_preference(&session_id)
            .ok()
            .flatten()
    });
    let candidates = match websocket_candidates(&state, preferred_provider.as_deref()) {
        Ok(candidates) if !candidates.is_empty() => candidates,
        Ok(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "当前分组没有配置 Responses WebSocket 的可用账号",
            )
                .into_response()
        }
        Err(message) => return (StatusCode::BAD_GATEWAY, message).into_response(),
    };
    let (candidate_index, provider, upstream) =
        match connect_candidate_websocket_from(&state, &candidates, 0, None).await {
            Ok(connected) => connected,
            Err(error) => return (StatusCode::BAD_GATEWAY, error).into_response(),
        };
    let provider_id = provider.id.clone();
    let bridge_state = state.clone();
    let mut response = websocket
        .on_upgrade(move |client| {
            bridge_websocket(
                client,
                bridge_state,
                candidates,
                candidate_index,
                provider,
                upstream,
                allowed_models,
            )
        })
        .into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&provider_id) {
        response
            .headers_mut()
            .insert("x-codex-companion-provider", value);
    }
    response
}

#[derive(Debug)]
struct WebSocketAuthError {
    status: StatusCode,
    message: String,
}

impl WebSocketAuthError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }
}

impl IntoResponse for WebSocketAuthError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

fn authenticate_websocket_client(
    state: &RelayState,
    headers: &HeaderMap,
) -> Result<Option<ApiClient>, WebSocketAuthError> {
    let config = state
        .store
        .load()
        .map_err(|error| WebSocketAuthError::internal(error.to_string()))?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        })
        .filter(|value| !value.is_empty());
    let api_client = token
        .map(|token| state.api_service.authenticate(token))
        .transpose()
        .map_err(|error| WebSocketAuthError::internal(error.to_string()))?
        .flatten();
    if api_client.is_none()
        && (config.relay.require_api_key
            || state.enforce_api_key
            || headers.contains_key(header::ORIGIN))
    {
        return Err(WebSocketAuthError::unauthorized(
            "WebSocket API key 无效或缺失",
        ));
    }
    Ok(api_client)
}

fn websocket_candidates(
    state: &RelayState,
    preferred_provider: Option<&str>,
) -> Result<Vec<ProviderConfig>, String> {
    let mut config = state.store.load().map_err(|error| error.to_string())?;
    if normalize_websocket_health(&mut config) {
        let _ = state.store.update(|current| {
            normalize_websocket_health(current);
            Ok(())
        });
    }
    let group = config
        .groups
        .get(&config.relay.active_group_id)
        .ok_or_else(|| "当前分组不存在".to_string())?;
    let mut selected = selected_providers_for_group(&config, group)
        .into_iter()
        .filter(|provider| provider.enabled && provider.websocket_url.is_some())
        .collect::<Vec<_>>();
    if let Some(preferred) = preferred_provider
        .filter(|provider_id| {
            group
                .provider_order
                .iter()
                .any(|candidate| candidate == provider_id)
        })
        .and_then(|provider_id| config.providers.get(provider_id))
        .filter(|provider| {
            provider.enabled
                && provider.websocket_url.is_some()
                && !selected.iter().any(|candidate| candidate.id == provider.id)
        })
    {
        selected.push(preferred.clone());
    }
    Ok(websocket_candidates_from_selected(
        state,
        &config,
        group,
        selected,
        preferred_provider,
    ))
}

fn normalize_websocket_health(config: &mut codex_companion_core::CompanionConfig) -> bool {
    let mut changed = false;
    for health in config.health.values_mut() {
        changed |= repair_legacy_auth_misclassification(health);
        let previous_status = health.status.clone();
        let previous_cooldown = health.cooldown_until;
        normalize_expired_cooldown(health);
        changed |= health.status != previous_status || health.cooldown_until != previous_cooldown;
    }
    changed
}

fn websocket_candidates_from_selected(
    state: &RelayState,
    config: &codex_companion_core::CompanionConfig,
    group: &codex_companion_core::ProviderGroup,
    selected: Vec<ProviderConfig>,
    preferred_provider: Option<&str>,
) -> Vec<ProviderConfig> {
    // 与 HTTP 路由保持同一健康策略：凭证已失效的账号不再尝试；短暂故障
    // 的冷却账号在健康账号之后保留为兜底，避免整个组都在冷却时直接断开。
    let has_alternatives = group.fallback_enabled && selected.len() > 1;
    let mut cooldown_probes = Vec::new();
    let mut candidates = selected
        .into_iter()
        .filter_map(|provider| {
            let health = config.health.get(&provider.id);
            if health.is_some_and(|health| matches!(health.status, HealthStatusKind::AuthFailed)) {
                return None;
            }
            if !has_alternatives || health.is_none_or(|health| !cooldown_active(health)) {
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

    apply_group_policy(state, group, &mut candidates);
    if let Some(index) = preferred_provider.and_then(|provider_id| {
        candidates
            .iter()
            .position(|provider| provider.id == provider_id)
    }) {
        let preferred = candidates.remove(index);
        candidates.insert(0, preferred);
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
    candidates
}

fn websocket_session_id(headers: &HeaderMap) -> Option<String> {
    [
        "session_id",
        "x-session-id",
        "x-amp-thread-id",
        "x-client-request-id",
    ]
    .into_iter()
    .find_map(|header| {
        headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[derive(Debug, Clone)]
struct WebSocketConnectError {
    message: String,
    status: Option<u16>,
}

impl WebSocketConnectError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: None,
        }
    }

    fn with_prefix(self, prefix: &str) -> Self {
        Self {
            message: format!("{prefix}: {}", self.message),
            status: self.status,
        }
    }
}

async fn connect_provider_websocket(
    state: &RelayState,
    provider: &ProviderConfig,
) -> Result<UpstreamWebSocket, WebSocketConnectError> {
    let mut agent_task_id = None;
    let authorization = if provider.kind == ProviderKind::OfficialCodex {
        if provider_uses_agent_identity(provider) {
            let auth = ensure_agent_identity_authorization(&state.client, provider, None)
                .await
                .map_err(|error| WebSocketConnectError::message(error.to_string()))?;
            agent_task_id = Some(auth.task_id);
            Some(auth.header)
        } else {
            let auth = ensure_codex_auth_snapshot(provider)
                .await
                .map_err(|error| WebSocketConnectError::message(error.to_string()))?;
            Some(format!("Bearer {}", auth.access_token))
        }
    } else {
        resolve_auth_token(provider).map(|token| format!("Bearer {token}"))
    };
    let request = websocket_request(provider, authorization.as_deref())
        .map_err(WebSocketConnectError::message)?;
    match connect_websocket_with_timeout(request).await {
        Ok(websocket) => Ok(websocket),
        Err(error)
            if error.status == Some(StatusCode::UNAUTHORIZED.as_u16())
                && agent_task_id.is_some() =>
        {
            let auth = ensure_agent_identity_authorization(
                &state.client,
                provider,
                agent_task_id.as_deref(),
            )
            .await
            .map_err(|error| WebSocketConnectError::message(error.to_string()))?;
            let request = websocket_request(provider, Some(&auth.header))
                .map_err(WebSocketConnectError::message)?;
            connect_websocket_with_timeout(request)
                .await
                .map_err(|error| error.with_prefix("WebSocket task 恢复后仍连接失败"))
        }
        Err(error) => Err(error),
    }
}

async fn connect_websocket_with_timeout(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
) -> Result<UpstreamWebSocket, WebSocketConnectError> {
    match tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request)).await {
        Err(_) => Err(WebSocketConnectError::message("WebSocket 连接超时")),
        Ok(Ok((websocket, _))) => Ok(websocket),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            let status = response.status();
            Err(WebSocketConnectError {
                message: format!("WebSocket 连接失败: HTTP {status}"),
                status: Some(status.as_u16()),
            })
        }
        Ok(Err(error)) => Err(WebSocketConnectError::message(format!(
            "WebSocket 连接失败: {error}"
        ))),
    }
}

#[cfg(test)]
async fn connect_candidate_websocket(
    state: &RelayState,
    candidates: Vec<ProviderConfig>,
) -> Result<(ProviderConfig, UpstreamWebSocket), String> {
    let (_, provider, upstream) =
        connect_candidate_websocket_from(state, &candidates, 0, None).await?;
    Ok((provider, upstream))
}

async fn connect_candidate_websocket_from(
    state: &RelayState,
    candidates: &[ProviderConfig],
    start_index: usize,
    replay: Option<&ClientMessage>,
) -> Result<(usize, ProviderConfig, UpstreamWebSocket), String> {
    let mut last_error = None;
    for (index, provider) in candidates.iter().enumerate().skip(start_index) {
        match connect_provider_websocket(state, provider).await {
            Ok(mut upstream) => {
                if let Some(frame) = replay {
                    if let Err(error) = upstream
                        .send(client_to_upstream_for_provider(frame.clone(), provider))
                        .await
                    {
                        let message = format!("WebSocket 重放请求失败: {error}");
                        record_websocket_failure(state, provider, None, &message);
                        last_error = Some(message);
                        continue;
                    }
                }
                return Ok((index, provider.clone(), upstream));
            }
            Err(error) => {
                record_websocket_failure(state, provider, error.status, &error.message);
                last_error = Some(error.message);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "所有 WebSocket provider 连接失败".to_string()))
}

async fn connect_next_websocket(
    state: &RelayState,
    candidates: &[ProviderConfig],
    current_index: usize,
    attempted: &mut HashSet<usize>,
    replay: Option<&ClientMessage>,
) -> Result<(usize, ProviderConfig, UpstreamWebSocket), String> {
    let mut last_error = None;
    for offset in 1..=candidates.len() {
        let index = (current_index + offset) % candidates.len();
        if !attempted.insert(index) {
            continue;
        }
        let provider = &candidates[index];
        match connect_provider_websocket(state, provider).await {
            Ok(mut upstream) => {
                if let Some(frame) = replay {
                    if let Err(error) = upstream
                        .send(client_to_upstream_for_provider(frame.clone(), provider))
                        .await
                    {
                        let message = format!("WebSocket 重放请求失败: {error}");
                        record_websocket_failure(state, provider, None, &message);
                        last_error = Some(message);
                        continue;
                    }
                }
                return Ok((index, provider.clone(), upstream));
            }
            Err(error) => {
                record_websocket_failure(state, provider, error.status, &error.message);
                last_error = Some(error.message);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "没有可用于 WebSocket 重试的 provider".to_string()))
}

fn websocket_request(
    provider: &ProviderConfig,
    authorization: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let url = provider
        .websocket_url
        .as_deref()
        .ok_or_else(|| "provider 未配置 websocket_url".to_string())?;
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("WebSocket URL 无效: {error}"))?;
    if let Some(authorization) = authorization {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(authorization)
                .map_err(|_| "WebSocket Authorization 无效".to_string())?,
        );
    }
    if let Some(account_id) = provider
        .account
        .as_ref()
        .and_then(|account| account.account_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        request.headers_mut().insert(
            "ChatGPT-Account-Id",
            HeaderValue::from_str(account_id).map_err(|_| "ChatGPT account id 无效".to_string())?,
        );
    }
    if provider.kind == ProviderKind::OfficialCodex {
        request
            .headers_mut()
            .insert("originator", HeaderValue::from_static("codex_cli_rs"));
        request
            .headers_mut()
            .insert("version", HeaderValue::from_static("0.144.1"));
    }
    Ok(request)
}

enum WebSocketBridgeEvent {
    Client(Option<Result<ClientMessage, axum::Error>>),
    Upstream(Option<Result<UpstreamMessage, tokio_tungstenite::tungstenite::Error>>),
    IdleTimeout,
}

async fn bridge_websocket(
    client: ClientWebSocket,
    state: RelayState,
    candidates: Vec<ProviderConfig>,
    mut candidate_index: usize,
    mut provider: ProviderConfig,
    upstream: UpstreamWebSocket,
    allowed_models: Vec<String>,
) {
    let (mut client_sink, mut client_stream) = client.split();
    let (upstream_sink, upstream_stream) = upstream.split();
    let mut upstream_sink = Some(upstream_sink);
    let mut upstream_stream = Some(upstream_stream);
    let mut pending = PendingWebSocketResponse::default();
    let mut attempted = HashSet::from([candidate_index]);
    let mut request_guard = state.begin_provider_request(&provider.id);
    let mut last_upstream_activity = tokio::time::Instant::now();

    loop {
        let event = if let Some(upstream) = upstream_stream.as_mut() {
            let idle_deadline = pending.request.as_ref().map(|_| {
                let idle_deadline = last_upstream_activity
                    + if pending.visible_output {
                        WEBSOCKET_STREAM_IDLE_TIMEOUT
                    } else {
                        WEBSOCKET_PREFLIGHT_IDLE_TIMEOUT
                    };
                if pending.visible_output {
                    idle_deadline
                } else {
                    pending
                        .started_at
                        .map(|started_at| {
                            idle_deadline.min(started_at + WEBSOCKET_PREFLIGHT_MAX_DURATION)
                        })
                        .unwrap_or(idle_deadline)
                }
            });
            let disabled_deadline = tokio::time::Instant::now() + Duration::from_secs(86_400);
            tokio::select! {
                client_message = client_stream.next() => WebSocketBridgeEvent::Client(client_message),
                upstream_message = upstream.next() => WebSocketBridgeEvent::Upstream(upstream_message),
                _ = tokio::time::sleep_until(idle_deadline.unwrap_or(disabled_deadline)), if idle_deadline.is_some() => WebSocketBridgeEvent::IdleTimeout,
            }
        } else {
            WebSocketBridgeEvent::Client(client_stream.next().await)
        };

        match event {
            WebSocketBridgeEvent::Client(client_message) => {
                let Some(Ok(message)) = client_message else {
                    break;
                };
                if let Some(model) = frame_disallowed_model(&message, &allowed_models) {
                    let error = serde_json::json!({
                        "type": "error",
                        "error": {
                            "code": "model_not_allowed",
                            "message": format!("API client 无权使用模型 {model}"),
                        },
                    })
                    .to_string();
                    let _ = client_sink.send(ClientMessage::Text(error.into())).await;
                    break;
                }
                let response_create = frame_has_type(&message, "response.create");
                let response_cancel = frame_has_type(&message, "response.cancel");
                let starts_tracked_response = response_create && pending.request.is_none();
                let close = matches!(message, ClientMessage::Close(_));
                if starts_tracked_response {
                    pending.begin(message.clone());
                    attempted.clear();
                    attempted.insert(candidate_index);
                    last_upstream_activity = tokio::time::Instant::now();
                }
                if response_cancel {
                    pending.clear();
                }
                if close {
                    break;
                }
                if upstream_sink.is_none() {
                    if response_cancel {
                        continue;
                    }
                    match reconnect_websocket_from_start(
                        &state,
                        &candidates,
                        &mut candidate_index,
                        &mut provider,
                        &mut attempted,
                        &mut request_guard,
                        &mut upstream_sink,
                        &mut upstream_stream,
                        Some(&message),
                    )
                    .await
                    {
                        Ok(()) => {
                            last_upstream_activity = tokio::time::Instant::now();
                            continue;
                        }
                        Err(error) => {
                            let detail =
                                format!("无法建立上游 WebSocket 连接以发送客户端帧: {error}");
                            if pending.request.is_some() {
                                if flush_pending_messages(&mut client_sink, &mut pending)
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                if client_sink
                                    .send(websocket_failed_event(&detail))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                pending.clear();
                            } else if client_sink
                                .send(websocket_transport_error_event(&detail))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    }
                }

                let send_result = upstream_sink
                    .as_mut()
                    .expect("upstream sink checked above")
                    .send(client_to_upstream_for_provider(message.clone(), &provider))
                    .await;
                if let Err(error) = send_result {
                    let detail = format!("向上游发送 WebSocket 帧失败: {error}");
                    discard_upstream_connection(&mut upstream_sink, &mut upstream_stream);
                    let may_replay =
                        pending.can_replay() && (!response_create || starts_tracked_response);
                    if may_replay {
                        if recover_websocket_before_output(
                            &state,
                            &candidates,
                            &mut candidate_index,
                            &mut provider,
                            &mut attempted,
                            &mut pending,
                            &mut request_guard,
                            &mut upstream_sink,
                            &mut upstream_stream,
                            None,
                            &detail,
                            true,
                        )
                        .await
                        {
                            last_upstream_activity = tokio::time::Instant::now();
                            continue;
                        }
                    } else {
                        record_websocket_failure(&state, &provider, None, &detail);
                    }

                    if pending.request.is_none() && !response_cancel {
                        match reconnect_websocket_from_start(
                            &state,
                            &candidates,
                            &mut candidate_index,
                            &mut provider,
                            &mut attempted,
                            &mut request_guard,
                            &mut upstream_sink,
                            &mut upstream_stream,
                            Some(&message),
                        )
                        .await
                        {
                            Ok(()) => {
                                last_upstream_activity = tokio::time::Instant::now();
                                continue;
                            }
                            Err(retry_error) => {
                                let detail = format!("{detail}; 重新连接失败: {retry_error}");
                                if client_sink
                                    .send(websocket_transport_error_event(&detail))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                    }

                    if pending.request.is_some() {
                        if flush_pending_messages(&mut client_sink, &mut pending)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if client_sink
                            .send(websocket_failed_event(&detail))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        pending.clear();
                    }
                }
            }
            WebSocketBridgeEvent::Upstream(upstream_message) => {
                let upstream_message = match upstream_message {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => {
                        let detail = format!("读取上游 WebSocket 帧失败: {error}");
                        if !handle_broken_upstream(
                            &state,
                            &candidates,
                            &mut candidate_index,
                            &mut provider,
                            &mut attempted,
                            &mut pending,
                            &mut request_guard,
                            &mut upstream_sink,
                            &mut upstream_stream,
                            &mut client_sink,
                            &detail,
                        )
                        .await
                        {
                            break;
                        }
                        last_upstream_activity = tokio::time::Instant::now();
                        continue;
                    }
                    None => {
                        let detail = "上游 WebSocket 在响应完成前断开";
                        if !handle_broken_upstream(
                            &state,
                            &candidates,
                            &mut candidate_index,
                            &mut provider,
                            &mut attempted,
                            &mut pending,
                            &mut request_guard,
                            &mut upstream_sink,
                            &mut upstream_stream,
                            &mut client_sink,
                            detail,
                        )
                        .await
                        {
                            break;
                        }
                        last_upstream_activity = tokio::time::Instant::now();
                        continue;
                    }
                };

                if matches!(
                    upstream_message,
                    UpstreamMessage::Ping(_) | UpstreamMessage::Pong(_)
                ) {
                    if client_sink
                        .send(upstream_to_client(upstream_message))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                if matches!(upstream_message, UpstreamMessage::Frame(_)) {
                    continue;
                }
                if matches!(upstream_message, UpstreamMessage::Close(_)) {
                    let detail = "上游 WebSocket 在响应完成前关闭";
                    if !handle_broken_upstream(
                        &state,
                        &candidates,
                        &mut candidate_index,
                        &mut provider,
                        &mut attempted,
                        &mut pending,
                        &mut request_guard,
                        &mut upstream_sink,
                        &mut upstream_stream,
                        &mut client_sink,
                        detail,
                    )
                    .await
                    {
                        break;
                    }
                    last_upstream_activity = tokio::time::Instant::now();
                    continue;
                }
                last_upstream_activity = tokio::time::Instant::now();

                let upstream_event = inspect_upstream_message(&upstream_message);
                let terminal_failure =
                    matches!(&upstream_event, WebSocketUpstreamEvent::Failure { .. });
                match upstream_event {
                    WebSocketUpstreamEvent::Failure { detail, status } => {
                        if pending.can_replay() {
                            if recover_websocket_before_output(
                                &state,
                                &candidates,
                                &mut candidate_index,
                                &mut provider,
                                &mut attempted,
                                &mut pending,
                                &mut request_guard,
                                &mut upstream_sink,
                                &mut upstream_stream,
                                status,
                                &detail,
                                false,
                            )
                            .await
                            {
                                last_upstream_activity = tokio::time::Instant::now();
                                continue;
                            }
                            if flush_pending_messages(&mut client_sink, &mut pending)
                                .await
                                .is_err()
                            {
                                break;
                            }
                            if client_sink
                                .send(upstream_to_client(upstream_message))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            pending.clear();
                            continue;
                        }
                        record_websocket_failure(&state, &provider, status, &detail);
                    }
                    WebSocketUpstreamEvent::OutputStarted => {
                        pending.mark_output_started();
                    }
                    WebSocketUpstreamEvent::Terminal => {
                        if flush_pending_messages(&mut client_sink, &mut pending)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if client_sink
                            .send(upstream_to_client(upstream_message))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        record_health_success(&state.store, &provider.id);
                        pending.clear();
                        attempted.clear();
                        continue;
                    }
                    WebSocketUpstreamEvent::Other => {}
                }

                if pending.request.is_some() && !pending.visible_output {
                    if !pending.buffer(upstream_message) {
                        let detail = "上游 WebSocket 在可见输出前发送了过多元数据";
                        if recover_websocket_before_output(
                            &state,
                            &candidates,
                            &mut candidate_index,
                            &mut provider,
                            &mut attempted,
                            &mut pending,
                            &mut request_guard,
                            &mut upstream_sink,
                            &mut upstream_stream,
                            None,
                            detail,
                            false,
                        )
                        .await
                        {
                            last_upstream_activity = tokio::time::Instant::now();
                            continue;
                        }
                        if client_sink
                            .send(websocket_failed_event(detail))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        pending.clear();
                    }
                    continue;
                }
                if pending.visible_output
                    && flush_pending_messages(&mut client_sink, &mut pending)
                        .await
                        .is_err()
                {
                    break;
                }
                if client_sink
                    .send(upstream_to_client(upstream_message))
                    .await
                    .is_err()
                {
                    break;
                }
                if terminal_failure {
                    pending.clear();
                }
            }
            WebSocketBridgeEvent::IdleTimeout => {
                let detail = if pending.visible_output {
                    "上游 WebSocket 在输出开始后空闲超时"
                } else {
                    "上游 WebSocket 在可见输出前空闲超时"
                };
                if !handle_broken_upstream(
                    &state,
                    &candidates,
                    &mut candidate_index,
                    &mut provider,
                    &mut attempted,
                    &mut pending,
                    &mut request_guard,
                    &mut upstream_sink,
                    &mut upstream_stream,
                    &mut client_sink,
                    detail,
                )
                .await
                {
                    break;
                }
                last_upstream_activity = tokio::time::Instant::now();
            }
        }
    }
    let _ = client_sink.send(ClientMessage::Close(None)).await;
    if let Some(mut upstream_sink) = upstream_sink {
        let _ = upstream_sink.send(UpstreamMessage::Close(None)).await;
    }
}

#[derive(Debug, Default)]
struct PendingWebSocketResponse {
    request: Option<ClientMessage>,
    started_at: Option<tokio::time::Instant>,
    visible_output: bool,
    prefetched: VecDeque<UpstreamMessage>,
    prefetched_bytes: usize,
}

impl PendingWebSocketResponse {
    fn begin(&mut self, request: ClientMessage) {
        self.request = Some(request);
        self.started_at = Some(tokio::time::Instant::now());
        self.visible_output = false;
        self.prefetched.clear();
        self.prefetched_bytes = 0;
    }

    fn can_replay(&self) -> bool {
        self.request.is_some() && !self.visible_output
    }

    fn mark_replayed(&mut self) {
        self.started_at = Some(tokio::time::Instant::now());
        self.visible_output = false;
        self.prefetched.clear();
        self.prefetched_bytes = 0;
    }

    fn reset_after_replay(&mut self) {
        self.mark_replayed();
    }

    fn mark_output_started(&mut self) {
        self.visible_output = true;
    }

    fn buffer(&mut self, message: UpstreamMessage) -> bool {
        let message_bytes = websocket_message_size(&message);
        if self.prefetched.len() >= MAX_WEBSOCKET_PREFLIGHT_MESSAGES
            || self.prefetched_bytes.saturating_add(message_bytes) > MAX_WEBSOCKET_PREFLIGHT_BYTES
        {
            return false;
        }
        self.prefetched_bytes = self.prefetched_bytes.saturating_add(message_bytes);
        self.prefetched.push_back(message);
        true
    }

    fn clear(&mut self) {
        self.request = None;
        self.started_at = None;
        self.visible_output = false;
        self.prefetched.clear();
        self.prefetched_bytes = 0;
    }
}

#[derive(Debug)]
enum WebSocketUpstreamEvent {
    Other,
    OutputStarted,
    Terminal,
    Failure { detail: String, status: Option<u16> },
}

#[allow(clippy::too_many_arguments)]
async fn recover_websocket_before_output(
    state: &RelayState,
    candidates: &[ProviderConfig],
    candidate_index: &mut usize,
    provider: &mut ProviderConfig,
    attempted: &mut HashSet<usize>,
    pending: &mut PendingWebSocketResponse,
    request_guard: &mut crate::state::ProviderRequestGuard,
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
    status: Option<u16>,
    detail: &str,
    allow_current_provider_reconnect: bool,
) -> bool {
    record_websocket_failure(state, provider, status, detail);
    if !pending.can_replay() {
        return false;
    }
    let replay = pending.request.as_ref();
    match connect_next_websocket(state, candidates, *candidate_index, attempted, replay).await {
        Ok((next_index, next_provider, next_upstream)) => {
            append_event(
                &state.store,
                "fallback",
                Some(provider.id.clone()),
                format!(
                    "WebSocket 上游失败，切换到 Provider {}: {detail}",
                    next_provider.id
                ),
            );
            install_websocket_connection(
                state,
                candidate_index,
                provider,
                request_guard,
                upstream_sink,
                upstream_stream,
                next_index,
                next_provider,
                next_upstream,
            );
            pending.reset_after_replay();
            true
        }
        Err(error) => {
            if allow_current_provider_reconnect
                && reconnect_current_websocket(
                    state,
                    candidate_index,
                    provider,
                    request_guard,
                    upstream_sink,
                    upstream_stream,
                    replay,
                )
                .await
            {
                append_event(
                    &state.store,
                    "fallback",
                    Some(provider.id.clone()),
                    format!(
                        "WebSocket 上游连接中断，重新连接 Provider {}: {detail}",
                        provider.id
                    ),
                );
                pending.reset_after_replay();
                return true;
            }
            append_event(
                &state.store,
                "error",
                Some(provider.id.clone()),
                format!("WebSocket fallback 失败: {error}"),
            );
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reconnect_current_websocket(
    state: &RelayState,
    candidate_index: &mut usize,
    provider: &mut ProviderConfig,
    request_guard: &mut crate::state::ProviderRequestGuard,
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
    replay: Option<&ClientMessage>,
) -> bool {
    let current_provider = provider.clone();
    let mut upstream = match connect_provider_websocket(state, &current_provider).await {
        Ok(upstream) => upstream,
        Err(error) => {
            record_websocket_failure(state, &current_provider, error.status, &error.message);
            return false;
        }
    };
    if let Some(frame) = replay {
        if let Err(error) = upstream
            .send(client_to_upstream_for_provider(
                frame.clone(),
                &current_provider,
            ))
            .await
        {
            let detail = format!("WebSocket 重放请求失败: {error}");
            record_websocket_failure(state, &current_provider, None, &detail);
            return false;
        }
    }
    let current_index = *candidate_index;
    install_websocket_connection(
        state,
        candidate_index,
        provider,
        request_guard,
        upstream_sink,
        upstream_stream,
        current_index,
        current_provider,
        upstream,
    );
    true
}

#[allow(clippy::too_many_arguments)]
async fn reconnect_websocket_from_start(
    state: &RelayState,
    candidates: &[ProviderConfig],
    candidate_index: &mut usize,
    provider: &mut ProviderConfig,
    attempted: &mut HashSet<usize>,
    request_guard: &mut crate::state::ProviderRequestGuard,
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
    replay: Option<&ClientMessage>,
) -> Result<(), String> {
    let (next_index, next_provider, next_upstream) =
        connect_candidate_websocket_from(state, candidates, 0, replay).await?;
    install_websocket_connection(
        state,
        candidate_index,
        provider,
        request_guard,
        upstream_sink,
        upstream_stream,
        next_index,
        next_provider,
        next_upstream,
    );
    attempted.clear();
    attempted.insert(next_index);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_websocket_connection(
    state: &RelayState,
    candidate_index: &mut usize,
    provider: &mut ProviderConfig,
    request_guard: &mut crate::state::ProviderRequestGuard,
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
    next_index: usize,
    next_provider: ProviderConfig,
    next_upstream: UpstreamWebSocket,
) {
    discard_upstream_connection(upstream_sink, upstream_stream);
    *candidate_index = next_index;
    *provider = next_provider;
    *request_guard = state.begin_provider_request(&provider.id);
    let (next_sink, next_stream) = next_upstream.split();
    *upstream_sink = Some(next_sink);
    *upstream_stream = Some(next_stream);
}

fn discard_upstream_connection(
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
) {
    let _ = upstream_sink.take();
    let _ = upstream_stream.take();
}

#[allow(clippy::too_many_arguments)]
async fn handle_broken_upstream(
    state: &RelayState,
    candidates: &[ProviderConfig],
    candidate_index: &mut usize,
    provider: &mut ProviderConfig,
    attempted: &mut HashSet<usize>,
    pending: &mut PendingWebSocketResponse,
    request_guard: &mut crate::state::ProviderRequestGuard,
    upstream_sink: &mut Option<UpstreamSink>,
    upstream_stream: &mut Option<UpstreamStream>,
    client_sink: &mut ClientSink,
    detail: &str,
) -> bool {
    discard_upstream_connection(upstream_sink, upstream_stream);
    let attempted_replay = pending.can_replay();
    if attempted_replay
        && recover_websocket_before_output(
            state,
            candidates,
            candidate_index,
            provider,
            attempted,
            pending,
            request_guard,
            upstream_sink,
            upstream_stream,
            None,
            detail,
            true,
        )
        .await
    {
        return true;
    }
    if pending.request.is_none() {
        return true;
    }
    if !attempted_replay {
        record_websocket_failure(state, provider, None, detail);
    }
    if flush_pending_messages(client_sink, pending).await.is_err() {
        return false;
    }
    if client_sink
        .send(websocket_failed_event(detail))
        .await
        .is_err()
    {
        return false;
    }
    pending.clear();
    true
}

async fn flush_pending_messages(
    client_sink: &mut ClientSink,
    pending: &mut PendingWebSocketResponse,
) -> Result<(), ()> {
    while let Some(message) = pending.prefetched.pop_front() {
        client_sink
            .send(upstream_to_client(message))
            .await
            .map_err(|_| ())?;
    }
    pending.prefetched_bytes = 0;
    Ok(())
}

fn record_websocket_failure(
    state: &RelayState,
    provider: &ProviderConfig,
    status: Option<u16>,
    detail: &str,
) {
    let failure = classify_failure(status, detail);
    if !matches!(
        failure.kind,
        codex_companion_core::HealthFailureKind::RequestRejected
    ) {
        update_health(&state.store, &provider.id, |health| {
            mark_failure(health, &failure, detail.to_string())
        });
    }
    append_event(
        &state.store,
        "error",
        Some(provider.id.clone()),
        format!("WebSocket Provider {}: {detail}", provider.name),
    );
}

fn inspect_upstream_message(message: &UpstreamMessage) -> WebSocketUpstreamEvent {
    let payload = match message {
        UpstreamMessage::Text(text) => text.as_bytes(),
        UpstreamMessage::Binary(bytes) => bytes.as_ref(),
        _ => return WebSocketUpstreamEvent::Other,
    };
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return WebSocketUpstreamEvent::Other;
    };
    if let Some(detail) = semantic_failure_message(&value) {
        return WebSocketUpstreamEvent::Failure {
            detail,
            status: websocket_event_status(&value),
        };
    }
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(event_type, "response.completed" | "response.incomplete")
        || value
            .pointer("/choices/0/finish_reason")
            .is_some_and(|reason| !reason.is_null())
    {
        return WebSocketUpstreamEvent::Terminal;
    }
    if response_event_has_visible_output(&value) {
        return WebSocketUpstreamEvent::OutputStarted;
    }
    WebSocketUpstreamEvent::Other
}

fn websocket_event_status(value: &Value) -> Option<u16> {
    [
        value.pointer("/response/error/status"),
        value.pointer("/error/status"),
        value.get("status_code"),
        value.get("status"),
    ]
    .into_iter()
    .flatten()
    .find_map(|status| {
        status
            .as_u64()
            .and_then(|status| u16::try_from(status).ok())
            .or_else(|| status.as_str().and_then(|status| status.parse().ok()))
    })
}

fn websocket_message_size(message: &UpstreamMessage) -> usize {
    match message {
        UpstreamMessage::Text(text) => text.len(),
        UpstreamMessage::Binary(bytes)
        | UpstreamMessage::Ping(bytes)
        | UpstreamMessage::Pong(bytes) => bytes.len(),
        UpstreamMessage::Close(_) | UpstreamMessage::Frame(_) => 0,
    }
}

fn websocket_failed_event(detail: &str) -> ClientMessage {
    let event = json!({
        "type": "response.failed",
        "response": {
            "id": "resp_codex_companion",
            "object": "response",
            "status": "failed",
            "output": [],
            "error": {
                "code": "upstream_websocket_failed",
                "message": detail,
            }
        }
    });
    ClientMessage::Text(event.to_string().into())
}

fn websocket_transport_error_event(detail: &str) -> ClientMessage {
    let event = json!({
        "type": "error",
        "error": {
            "code": "upstream_websocket_unavailable",
            "message": detail,
        }
    });
    ClientMessage::Text(event.to_string().into())
}

/// 白名单非空时，检查客户端帧里请求的模型是否越权；返回越权的模型名。
fn frame_disallowed_model(message: &ClientMessage, allowed_models: &[String]) -> Option<String> {
    if allowed_models.is_empty() {
        return None;
    }
    let payload: &[u8] = match message {
        ClientMessage::Text(text) => text.as_bytes(),
        ClientMessage::Binary(bytes) => bytes,
        _ => return None,
    };
    let value = serde_json::from_slice::<serde_json::Value>(payload).ok()?;
    // 校验帧内所有可能生效的 model 字段，任一越权即拒绝；只取第一个存在的
    // 字段会被 {"model":合法,"response":{"model":越权}} 这类嵌套载荷绕过。
    let candidates = [
        value.get("model"),
        value
            .get("response")
            .and_then(|response| response.get("model")),
        value
            .get("session")
            .and_then(|session| session.get("model")),
    ];
    for candidate in candidates.into_iter().flatten() {
        let Some(model) = candidate
            .as_str()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        if !allowed_models.iter().any(|allowed| allowed == model) {
            return Some(model.to_string());
        }
    }
    None
}

#[cfg(test)]
fn client_to_upstream(message: ClientMessage, provider_kind: &ProviderKind) -> UpstreamMessage {
    let message = if *provider_kind == ProviderKind::OfficialCodex {
        normalize_official_client_frame(message)
    } else {
        message
    };
    match message {
        ClientMessage::Text(text) => UpstreamMessage::Text(text.to_string().into()),
        ClientMessage::Binary(bytes) => UpstreamMessage::Binary(bytes),
        ClientMessage::Ping(bytes) => UpstreamMessage::Ping(bytes),
        ClientMessage::Pong(bytes) => UpstreamMessage::Pong(bytes),
        ClientMessage::Close(_) => UpstreamMessage::Close(None),
    }
}

fn client_to_upstream_for_provider(
    message: ClientMessage,
    provider: &ProviderConfig,
) -> UpstreamMessage {
    client_message_to_upstream(normalize_client_frame_for_provider(message, provider))
}

fn client_message_to_upstream(message: ClientMessage) -> UpstreamMessage {
    match message {
        ClientMessage::Text(text) => UpstreamMessage::Text(text.to_string().into()),
        ClientMessage::Binary(bytes) => UpstreamMessage::Binary(bytes),
        ClientMessage::Ping(bytes) => UpstreamMessage::Ping(bytes),
        ClientMessage::Pong(bytes) => UpstreamMessage::Pong(bytes),
        ClientMessage::Close(_) => UpstreamMessage::Close(None),
    }
}

fn normalize_client_frame_for_provider(
    message: ClientMessage,
    provider: &ProviderConfig,
) -> ClientMessage {
    match message {
        ClientMessage::Text(text) => {
            let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
                return ClientMessage::Text(text);
            };
            if !normalize_client_json_for_provider(&mut value, provider) {
                return ClientMessage::Text(text);
            }
            ClientMessage::Text(value.to_string().into())
        }
        ClientMessage::Binary(bytes) => {
            let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
                return ClientMessage::Binary(bytes);
            };
            if !normalize_client_json_for_provider(&mut value, provider) {
                return ClientMessage::Binary(bytes);
            }
            serde_json::to_vec(&value)
                .map(|value| ClientMessage::Binary(value.into()))
                .unwrap_or(ClientMessage::Binary(bytes))
        }
        message => message,
    }
}

fn normalize_client_json_for_provider(value: &mut Value, provider: &ProviderConfig) -> bool {
    let mut changed = false;
    if provider.kind == ProviderKind::OfficialCodex {
        changed |= normalize_official_input_item_ids(value);
    }
    for pointer in ["/model", "/response/model", "/session/model"] {
        changed |= rewrite_websocket_model(value, pointer, provider);
    }
    for pointer in [
        "/reasoning/effort",
        "/reasoning_effort",
        "/response/reasoning/effort",
        "/response/reasoning_effort",
        "/session/reasoning/effort",
        "/session/reasoning_effort",
    ] {
        if let Some(effort) = value.pointer_mut(pointer) {
            if effort.as_str() == Some("ultra") {
                *effort = Value::String("max".to_string());
                changed = true;
            }
        }
    }
    changed
}

fn rewrite_websocket_model(value: &mut Value, pointer: &str, provider: &ProviderConfig) -> bool {
    let Some(model) = value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return false;
    };
    let Some(mapped) = provider.model_map.get(model) else {
        return false;
    };
    if mapped == model {
        return false;
    }
    if let Some(target) = value.pointer_mut(pointer) {
        *target = Value::String(mapped.clone());
        return true;
    }
    false
}

fn frame_has_type(message: &ClientMessage, expected: &str) -> bool {
    let payload: &[u8] = match message {
        ClientMessage::Text(text) => text.as_bytes(),
        ClientMessage::Binary(bytes) => bytes,
        _ => return false,
    };
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|event_type| event_type == expected)
}

#[cfg(test)]
fn normalize_official_client_frame(message: ClientMessage) -> ClientMessage {
    match message {
        ClientMessage::Text(text) => {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) else {
                return ClientMessage::Text(text);
            };
            if !normalize_official_input_item_ids(&mut value) {
                return ClientMessage::Text(text);
            }
            ClientMessage::Text(value.to_string().into())
        }
        ClientMessage::Binary(bytes) => {
            let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return ClientMessage::Binary(bytes);
            };
            if !normalize_official_input_item_ids(&mut value) {
                return ClientMessage::Binary(bytes);
            }
            serde_json::to_vec(&value)
                .map(|value| ClientMessage::Binary(value.into()))
                .unwrap_or(ClientMessage::Binary(bytes))
        }
        message => message,
    }
}

fn upstream_to_client(message: UpstreamMessage) -> ClientMessage {
    match message {
        UpstreamMessage::Text(text) => ClientMessage::Text(text.to_string().into()),
        UpstreamMessage::Binary(bytes) => ClientMessage::Binary(bytes),
        UpstreamMessage::Ping(bytes) => ClientMessage::Ping(bytes),
        UpstreamMessage::Pong(bytes) => ClientMessage::Pong(bytes),
        UpstreamMessage::Close(_) => ClientMessage::Close(None),
        UpstreamMessage::Frame(_) => ClientMessage::Close(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use codex_companion_core::{
        default_refresh_interval_seconds, ApiClientCreate, ConfigStore, ProviderAccountInfo,
        ProviderGroup, RelayConfig,
    };
    use std::collections::BTreeMap;
    use tokio_tungstenite::accept_async;

    fn provider(id: &str, websocket_url: Option<String>) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            websocket_url,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    fn state_with_group(providers: Vec<ProviderConfig>) -> RelayState {
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
                config.relay = RelayConfig {
                    active_group_id: "test".to_string(),
                    ..RelayConfig::default()
                };
                config.groups.insert(
                    "test".to_string(),
                    ProviderGroup {
                        id: "test".to_string(),
                        name: "Test".to_string(),
                        policy: codex_companion_core::GroupPolicy::PriorityFallback,
                        provider_order,
                        provider_weights: BTreeMap::new(),
                        fallback_enabled: true,
                        priority_failback_interval_seconds: 0,
                        priority_failback_revision: 0,
                        priority_failback_target_provider_id: None,
                    },
                );
                Ok(())
            })
            .expect("config");
        RelayState::new(store, reqwest::Client::new())
    }

    #[test]
    fn official_websocket_request_contains_identity_headers() {
        let mut provider = provider(
            "official",
            Some("wss://chatgpt.com/backend-api/codex/responses".to_string()),
        );
        provider.kind = ProviderKind::OfficialCodex;
        provider.account = Some(ProviderAccountInfo {
            account_id: Some("account-123".to_string()),
            ..ProviderAccountInfo::default()
        });

        let request = websocket_request(&provider, Some("Bearer test-token")).expect("request");

        assert_eq!(request.uri().scheme_str(), Some("wss"));
        assert_eq!(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-token")
        );
        assert_eq!(
            request
                .headers()
                .get("ChatGPT-Account-Id")
                .and_then(|value| value.to_str().ok()),
            Some("account-123")
        );
        assert_eq!(
            request
                .headers()
                .get("originator")
                .and_then(|value| value.to_str().ok()),
            Some("codex_cli_rs")
        );
        assert_eq!(
            request
                .headers()
                .get("version")
                .and_then(|value| value.to_str().ok()),
            Some("0.144.1")
        );
    }

    #[test]
    fn websocket_model_allowlist_blocks_disallowed_models() {
        let allowed = vec!["model-a".to_string()];
        let ok = ClientMessage::Text(r#"{"model":"model-a"}"#.into());
        let blocked = ClientMessage::Text(r#"{"model":"model-b"}"#.into());
        let nested = ClientMessage::Text(r#"{"response":{"model":"model-b"}}"#.into());
        // 攻击载荷：顶层放合法模型掩护，嵌套字段夹带越权模型。
        let smuggled =
            ClientMessage::Text(r#"{"model":"model-a","response":{"model":"model-b"}}"#.into());
        let smuggled_session =
            ClientMessage::Text(r#"{"model":"model-a","session":{"model":"model-b"}}"#.into());
        let no_model = ClientMessage::Text(r#"{"type":"ping"}"#.into());

        assert_eq!(frame_disallowed_model(&ok, &allowed), None);
        assert_eq!(
            frame_disallowed_model(&blocked, &allowed),
            Some("model-b".to_string())
        );
        assert_eq!(
            frame_disallowed_model(&nested, &allowed),
            Some("model-b".to_string())
        );
        assert_eq!(
            frame_disallowed_model(&smuggled, &allowed),
            Some("model-b".to_string())
        );
        assert_eq!(
            frame_disallowed_model(&smuggled_session, &allowed),
            Some("model-b".to_string())
        );
        assert_eq!(frame_disallowed_model(&no_model, &allowed), None);
        assert_eq!(frame_disallowed_model(&blocked, &[]), None);
    }

    #[test]
    fn official_websocket_frames_normalize_cross_provider_input_item_ids() {
        let message = ClientMessage::Text(
            r#"{"type":"response.create","response":{"model":"gpt-test","input":[{"type":"custom_tool_call","id":"item_99fb83474df510b04e475dc5","call_id":"call_1","name":"exec","input":""}]}}"#
                .into(),
        );

        let UpstreamMessage::Text(text) = client_to_upstream(message, &ProviderKind::OfficialCodex)
        else {
            panic!("expected text frame");
        };
        let value: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert_eq!(
            value["response"]["input"][0]["id"],
            "ctc_99fb83474df510b04e475dc5"
        );
    }

    #[test]
    fn websocket_frames_rewrite_nested_models_and_ultra_effort() {
        let mut provider = provider("relay", None);
        provider
            .model_map
            .insert("gpt-5.6-sol".to_string(), "upstream-model".to_string());
        let message = ClientMessage::Text(
            r#"{"type":"response.create","model":"gpt-5.6-sol","response":{"model":"gpt-5.6-sol","reasoning":{"effort":"ultra"}},"session":{"model":"gpt-5.6-sol"},"reasoning_effort":"ultra"}"#.into(),
        );

        let UpstreamMessage::Text(text) = client_to_upstream_for_provider(message, &provider)
        else {
            panic!("expected text frame");
        };
        let value: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["model"], "upstream-model");
        assert_eq!(value["response"]["model"], "upstream-model");
        assert_eq!(value["session"]["model"], "upstream-model");
        assert_eq!(value["reasoning_effort"], "max");
        assert_eq!(value["response"]["reasoning"]["effort"], "max");
    }

    #[test]
    fn compatible_websocket_frames_preserve_provider_specific_item_ids() {
        let original = r#"{"type":"response.create","response":{"input":[{"type":"custom_tool_call","id":"item_custom"}]}}"#;
        let message = ClientMessage::Text(original.into());

        let UpstreamMessage::Text(text) =
            client_to_upstream(message, &ProviderKind::OpenAiCompatible)
        else {
            panic!("expected text frame");
        };
        assert_eq!(text.as_str(), original);
    }

    #[test]
    fn websocket_client_authentication_enforces_configured_api_keys() {
        let state = state_with_group(Vec::new());
        state
            .store
            .update(|config| {
                config.relay.require_api_key = true;
                Ok(())
            })
            .expect("strict mode");
        let secret = state
            .api_service
            .create_client(ApiClientCreate {
                name: "WebSocket test".to_string(),
                allowed_models: Vec::new(),
            })
            .expect("client");

        let missing =
            authenticate_websocket_client(&state, &HeaderMap::new()).expect_err("missing key");
        assert_eq!(missing.status, StatusCode::UNAUTHORIZED);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {}", secret.api_key))
                .expect("header"),
        );
        authenticate_websocket_client(&state, &headers).expect("valid key");
    }

    #[test]
    fn websocket_authentication_honors_the_runtime_api_key_floor() {
        let state = state_with_group(Vec::new());
        let state =
            RelayState::new_with_api_key_floor(state.store.clone(), reqwest::Client::new(), true);
        assert!(!state.store.load().expect("config").relay.require_api_key);

        let missing =
            authenticate_websocket_client(&state, &HeaderMap::new()).expect_err("missing key");
        assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn websocket_session_preference_prioritizes_a_group_candidate() {
        let state = state_with_group(vec![
            provider("a", Some("ws://127.0.0.1:1/a".to_string())),
            provider("b", Some("ws://127.0.0.1:1/b".to_string())),
        ]);
        let candidates = websocket_candidates(&state, Some("b")).expect("candidates");
        assert_eq!(
            candidates
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );

        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("thread-b"));
        assert_eq!(websocket_session_id(&headers).as_deref(), Some("thread-b"));

        headers.insert("x-amp-thread-id", HeaderValue::from_static("thread-amp"));
        headers.insert(
            "x-client-request-id",
            HeaderValue::from_static("request-id"),
        );
        assert_eq!(websocket_session_id(&headers).as_deref(), Some("thread-b"));

        headers.remove("x-session-id");
        assert_eq!(
            websocket_session_id(&headers).as_deref(),
            Some("thread-amp")
        );
    }

    #[test]
    fn websocket_candidates_skip_invalid_credentials_and_deprioritize_cooldowns() {
        let state = state_with_group(vec![
            provider("invalid", Some("ws://127.0.0.1:1/invalid".to_string())),
            provider("cooling", Some("ws://127.0.0.1:1/cooling".to_string())),
            provider("healthy", Some("ws://127.0.0.1:1/healthy".to_string())),
        ]);
        state
            .store
            .update(|config| {
                config.health.insert(
                    "invalid".to_string(),
                    codex_companion_core::ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        last_failure_kind: Some(HealthFailureKind::AuthFailed),
                        ..Default::default()
                    },
                );
                config.health.insert(
                    "cooling".to_string(),
                    codex_companion_core::ProviderHealth {
                        status: HealthStatusKind::Cooldown,
                        cooldown_until: Some(chrono::Utc::now() + chrono::Duration::minutes(1)),
                        last_failure_kind: Some(HealthFailureKind::RateLimited),
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("health");

        let candidates = websocket_candidates(&state, None).expect("candidates");

        assert_eq!(
            candidates
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["healthy", "cooling"]
        );
    }

    #[test]
    fn websocket_candidates_repair_legacy_auth_misclassification() {
        let state = state_with_group(vec![provider(
            "legacy",
            Some("ws://127.0.0.1:1/legacy".to_string()),
        )]);
        state
            .store
            .update(|config| {
                config.health.insert(
                    "legacy".to_string(),
                    codex_companion_core::ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        last_error: Some("content policy violation".to_string()),
                        last_failure_kind: Some(HealthFailureKind::AuthFailed),
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .expect("legacy health");

        let candidates = websocket_candidates(&state, None).expect("candidates");

        assert_eq!(
            candidates
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["legacy"]
        );
        assert_eq!(
            state
                .store
                .load()
                .expect("config")
                .health
                .get("legacy")
                .map(|health| health.status.clone()),
            Some(HealthStatusKind::Unknown)
        );
    }

    #[tokio::test]
    async fn websocket_connection_falls_back_to_the_next_candidate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut websocket = accept_async(stream).await.expect("handshake");
            let _ = websocket.close(None).await;
        });
        let state = state_with_group(vec![
            provider(
                "unavailable",
                Some("ws://127.0.0.1:1/v1/responses".to_string()),
            ),
            provider("healthy", Some(format!("ws://{addr}/v1/responses"))),
        ]);
        let candidates = websocket_candidates(&state, None).expect("candidates");

        let (selected, mut upstream) = connect_candidate_websocket(&state, candidates)
            .await
            .expect("fallback connection");

        assert_eq!(selected.id, "healthy");
        let _ = upstream.close(None).await;
        server.await.expect("server");
    }

    #[tokio::test]
    async fn websocket_replays_pre_output_failure_on_the_next_provider() {
        let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("first bind");
        let first_addr = first_listener.local_addr().expect("first address");
        let first = tokio::spawn(async move {
            let (stream, _) = first_listener.accept().await.expect("first accept");
            let mut websocket = accept_async(stream).await.expect("first handshake");
            let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                panic!("first provider did not receive response.create");
            };
            assert!(frame.contains("response.create"));
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.failed","response":{"status":"failed","error":{"status":429,"message":"upstream capacity temporarily unavailable"}}}"#.into(),
                ))
                .await
                .expect("send failure");
            let _ = websocket.close(None).await;
        });

        let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("second bind");
        let second_addr = second_listener.local_addr().expect("second address");
        let second = tokio::spawn(async move {
            let (stream, _) = second_listener.accept().await.expect("second accept");
            let mut websocket = accept_async(stream).await.expect("second handshake");
            let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                panic!("second provider did not receive replay");
            };
            assert!(frame.contains("response.create"));
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.output_text.delta","delta":"from second"}"#.into(),
                ))
                .await
                .expect("send output");
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.completed","response":{"status":"completed"}}"#.into(),
                ))
                .await
                .expect("send completed");
        });

        let state = state_with_group(vec![
            provider("first", Some(format!("ws://{first_addr}/v1/responses"))),
            provider("second", Some(format!("ws://{second_addr}/v1/responses"))),
        ]);
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay address");
        let app = Router::new()
            .route("/v1/responses", get(responses_websocket))
            .with_state(state.clone());
        let relay = tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let (mut client, _) = connect_async(format!("ws://{relay_addr}/v1/responses"))
            .await
            .expect("relay handshake");
        client
            .send(UpstreamMessage::Text(
                r#"{"type":"response.create","response":{"model":"gpt-test","input":"hello"}}"#
                    .into(),
            ))
            .await
            .expect("send request");

        let mut output = String::new();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                .await
                .expect("client response timeout")
                .expect("client closed")
                .expect("client websocket error");
            if let UpstreamMessage::Text(text) = message {
                output.push_str(&text);
                if text.contains("response.completed") {
                    break;
                }
            }
        }
        assert!(output.contains("from second"));
        assert!(!output.contains("upstream capacity temporarily unavailable"));
        assert_eq!(
            state
                .store
                .load()
                .expect("config")
                .health
                .get("first")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(codex_companion_core::HealthFailureKind::RateLimited)
        );

        let _ = client.close(None).await;
        first.await.expect("first provider task");
        second.await.expect("second provider task");
        relay.abort();
    }

    #[tokio::test]
    async fn websocket_replays_pre_output_disconnect_on_the_next_provider() {
        let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("first bind");
        let first_addr = first_listener.local_addr().expect("first address");
        let first = tokio::spawn(async move {
            let (stream, _) = first_listener.accept().await.expect("first accept");
            let mut websocket = accept_async(stream).await.expect("first handshake");
            let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                panic!("first provider did not receive response.create");
            };
            assert!(frame.contains("response.create"));
            websocket.close(None).await.expect("close first provider");
        });

        let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("second bind");
        let second_addr = second_listener.local_addr().expect("second address");
        let second = tokio::spawn(async move {
            let (stream, _) = second_listener.accept().await.expect("second accept");
            let mut websocket = accept_async(stream).await.expect("second handshake");
            let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                panic!("second provider did not receive replay");
            };
            assert!(frame.contains("response.create"));
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.output_text.delta","delta":"from second after disconnect"}"#
                        .into(),
                ))
                .await
                .expect("send output");
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.completed","response":{"status":"completed"}}"#.into(),
                ))
                .await
                .expect("send completed");
        });

        let state = state_with_group(vec![
            provider("first", Some(format!("ws://{first_addr}/v1/responses"))),
            provider("second", Some(format!("ws://{second_addr}/v1/responses"))),
        ]);
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay address");
        let app = Router::new()
            .route("/v1/responses", get(responses_websocket))
            .with_state(state);
        let relay = tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let (mut client, _) = connect_async(format!("ws://{relay_addr}/v1/responses"))
            .await
            .expect("relay handshake");
        client
            .send(UpstreamMessage::Text(
                r#"{"type":"response.create","response":{"model":"gpt-test","input":"hello"}}"#
                    .into(),
            ))
            .await
            .expect("send request");

        let mut output = String::new();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                .await
                .expect("client response timeout")
                .expect("client closed")
                .expect("client websocket error");
            if let UpstreamMessage::Text(text) = message {
                output.push_str(&text);
                if text.contains("response.completed") {
                    break;
                }
            }
        }
        assert!(output.contains("from second after disconnect"));
        assert!(!output.contains("response.failed"));

        let _ = client.close(None).await;
        first.await.expect("first provider task");
        second.await.expect("second provider task");
        relay.abort();
    }

    #[tokio::test]
    async fn websocket_reconnects_after_upstream_closes_between_responses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("provider bind");
        let provider_addr = listener.local_addr().expect("provider address");
        let provider_server = tokio::spawn(async move {
            for (input, output) in [
                ("first request", "first output"),
                ("second request", "second output"),
            ] {
                let (stream, _) = listener.accept().await.expect("provider accept");
                let mut websocket = accept_async(stream).await.expect("provider handshake");
                let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                    panic!("provider did not receive response.create");
                };
                assert!(frame.contains(input));
                websocket
                    .send(UpstreamMessage::Text(
                        format!(r#"{{"type":"response.output_text.delta","delta":"{output}"}}"#)
                            .into(),
                    ))
                    .await
                    .expect("send output");
                websocket
                    .send(UpstreamMessage::Text(
                        r#"{"type":"response.completed","response":{"status":"completed"}}"#.into(),
                    ))
                    .await
                    .expect("send completed");
                websocket.close(None).await.expect("close provider");
            }
        });

        let state = state_with_group(vec![provider(
            "only",
            Some(format!("ws://{provider_addr}/v1/responses")),
        )]);
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay address");
        let app = Router::new()
            .route("/v1/responses", get(responses_websocket))
            .with_state(state);
        let relay = tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let (mut client, _) = connect_async(format!("ws://{relay_addr}/v1/responses"))
            .await
            .expect("relay handshake");
        for (input, expected_output) in [
            ("first request", "first output"),
            ("second request", "second output"),
        ] {
            client
                .send(UpstreamMessage::Text(
                    format!(
                        r#"{{"type":"response.create","response":{{"model":"gpt-test","input":"{input}"}}}}"#
                    )
                    .into(),
                ))
                .await
                .expect("send request");
            let mut output = String::new();
            loop {
                let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                    .await
                    .expect("client response timeout")
                    .expect("client closed")
                    .expect("client websocket error");
                if let UpstreamMessage::Text(text) = message {
                    output.push_str(&text);
                    if text.contains("response.completed") {
                        break;
                    }
                }
            }
            assert!(output.contains(expected_output));
        }

        let _ = client.close(None).await;
        provider_server.await.expect("provider task");
        relay.abort();
    }

    #[tokio::test]
    async fn websocket_does_not_replay_after_visible_output_disconnect() {
        let first_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("first bind");
        let first_addr = first_listener.local_addr().expect("first address");
        let first = tokio::spawn(async move {
            let (stream, _) = first_listener.accept().await.expect("first accept");
            let mut websocket = accept_async(stream).await.expect("first handshake");
            let Some(Ok(UpstreamMessage::Text(frame))) = websocket.next().await else {
                panic!("first provider did not receive response.create");
            };
            assert!(frame.contains("response.create"));
            websocket
                .send(UpstreamMessage::Text(
                    r#"{"type":"response.output_text.delta","delta":"partial output"}"#.into(),
                ))
                .await
                .expect("send output");
            websocket.close(None).await.expect("close first provider");
        });

        let second_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("second bind");
        let second_addr = second_listener.local_addr().expect("second address");
        let second_was_used = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(300), second_listener.accept())
                .await
                .is_ok()
        });

        let state = state_with_group(vec![
            provider("first", Some(format!("ws://{first_addr}/v1/responses"))),
            provider("second", Some(format!("ws://{second_addr}/v1/responses"))),
        ]);
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay address");
        let app = Router::new()
            .route("/v1/responses", get(responses_websocket))
            .with_state(state);
        let relay = tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let (mut client, _) = connect_async(format!("ws://{relay_addr}/v1/responses"))
            .await
            .expect("relay handshake");
        client
            .send(UpstreamMessage::Text(
                r#"{"type":"response.create","response":{"model":"gpt-test","input":"hello"}}"#
                    .into(),
            ))
            .await
            .expect("send request");

        let mut output = String::new();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                .await
                .expect("client response timeout")
                .expect("client closed")
                .expect("client websocket error");
            if let UpstreamMessage::Text(text) = message {
                output.push_str(&text);
                if text.contains("response.failed") {
                    break;
                }
            }
        }
        assert!(output.contains("partial output"));
        assert!(output.contains("response.failed"));
        assert!(!second_was_used.await.expect("second listener task"));

        let _ = client.close(None).await;
        first.await.expect("first provider task");
        relay.abort();
    }
}
