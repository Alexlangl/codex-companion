use crate::state::{apply_group_policy, RelayState};
use axum::{
    extract::{ws::Message as ClientMessage, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use codex_companion_core::{ApiClient, ProviderConfig, ProviderKind};
use codex_companion_provider::{
    ensure_agent_identity_authorization, ensure_codex_auth_snapshot, provider_uses_agent_identity,
    resolve_auth_token, selected_providers_for_group,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as UpstreamMessage},
    MaybeTlsStream, WebSocketStream,
};

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
    let (provider, upstream) = match connect_candidate_websocket(&state, candidates).await {
        Ok(connected) => connected,
        Err(error) => return (StatusCode::BAD_GATEWAY, error).into_response(),
    };
    let provider_id = provider.id.clone();
    let mut response = websocket
        .on_upgrade(move |client| bridge_websocket(client, upstream, allowed_models))
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
    let config = state.store.load().map_err(|error| error.to_string())?;
    let group = config
        .groups
        .get(&config.relay.active_group_id)
        .ok_or_else(|| "当前分组不存在".to_string())?;
    let mut candidates = selected_providers_for_group(&config, group)
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
                && !candidates
                    .iter()
                    .any(|candidate| candidate.id == provider.id)
        })
    {
        candidates.push(preferred.clone());
    }
    apply_group_policy(state, group, &mut candidates);
    if let Some(index) = preferred_provider.and_then(|provider_id| {
        candidates
            .iter()
            .position(|provider| provider.id == provider_id)
    }) {
        let preferred = candidates.remove(index);
        candidates.insert(0, preferred);
    }
    if !group.fallback_enabled {
        candidates.truncate(1);
    }
    Ok(candidates)
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

async fn connect_provider_websocket(
    state: &RelayState,
    provider: &ProviderConfig,
) -> Result<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, String> {
    let mut agent_task_id = None;
    let authorization = if provider.kind == ProviderKind::OfficialCodex {
        if provider_uses_agent_identity(provider) {
            let auth = ensure_agent_identity_authorization(&state.client, provider, None)
                .await
                .map_err(|error| error.to_string())?;
            agent_task_id = Some(auth.task_id);
            Some(auth.header)
        } else {
            let auth = ensure_codex_auth_snapshot(provider)
                .await
                .map_err(|error| error.to_string())?;
            Some(format!("Bearer {}", auth.access_token))
        }
    } else {
        resolve_auth_token(provider).map(|token| format!("Bearer {token}"))
    };
    let request = websocket_request(provider, authorization.as_deref())?;
    match connect_async(request).await {
        Ok((websocket, _)) => Ok(websocket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response))
            if response.status() == StatusCode::UNAUTHORIZED && agent_task_id.is_some() =>
        {
            let auth = ensure_agent_identity_authorization(
                &state.client,
                provider,
                agent_task_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let request = websocket_request(provider, Some(&auth.header))?;
            connect_async(request)
                .await
                .map(|(websocket, _)| websocket)
                .map_err(|error| format!("WebSocket task 恢复后仍连接失败: {error}"))
        }
        Err(error) => Err(format!("WebSocket 连接失败: {error}")),
    }
}

async fn connect_candidate_websocket(
    state: &RelayState,
    candidates: Vec<ProviderConfig>,
) -> Result<
    (
        ProviderConfig,
        WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    ),
    String,
> {
    let mut last_error = None;
    for provider in candidates {
        match connect_provider_websocket(state, &provider).await {
            Ok(upstream) => return Ok((provider, upstream)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "所有 WebSocket provider 连接失败".to_string()))
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

async fn bridge_websocket(
    client: axum::extract::ws::WebSocket,
    upstream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    allowed_models: Vec<String>,
) {
    let (mut client_sink, mut client_stream) = client.split();
    let (mut upstream_sink, mut upstream_stream) = upstream.split();
    loop {
        tokio::select! {
            client_message = client_stream.next() => {
                let Some(Ok(message)) = client_message else { break; };
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
                let close = matches!(message, ClientMessage::Close(_));
                if upstream_sink.send(client_to_upstream(message)).await.is_err() || close {
                    break;
                }
            }
            upstream_message = upstream_stream.next() => {
                let Some(Ok(message)) = upstream_message else { break; };
                let close = matches!(message, UpstreamMessage::Close(_));
                if client_sink.send(upstream_to_client(message)).await.is_err() || close {
                    break;
                }
            }
        }
    }
    let _ = client_sink.send(ClientMessage::Close(None)).await;
    let _ = upstream_sink.send(UpstreamMessage::Close(None)).await;
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

fn client_to_upstream(message: ClientMessage) -> UpstreamMessage {
    match message {
        ClientMessage::Text(text) => UpstreamMessage::Text(text.to_string().into()),
        ClientMessage::Binary(bytes) => UpstreamMessage::Binary(bytes),
        ClientMessage::Ping(bytes) => UpstreamMessage::Ping(bytes),
        ClientMessage::Pong(bytes) => UpstreamMessage::Pong(bytes),
        ClientMessage::Close(_) => UpstreamMessage::Close(None),
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
}
