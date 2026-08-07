use crate::api_service::ApiServiceStore;
use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::{
    provider_base_url_is_endpoint, provider_endpoint_is_chat_completions, ConfigStore,
    ProviderConfig, ProviderKind,
};
use codex_companion_health::{classify_failure, mark_failure};
use codex_companion_provider::{
    ensure_agent_identity_authorization, ensure_codex_auth_snapshot,
    is_agent_identity_task_invalid, provider_uses_agent_identity, redact_agent_identity_body,
    resolve_auth_token,
};
use futures_util::{stream, Stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::{fmt, io, time::Duration};

const STREAM_PREFLIGHT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_STREAM_PREFLIGHT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct UpstreamResponse {
    response: Option<reqwest::Response>,
    buffered_body: Option<Bytes>,
    prefetched_body: VecDeque<Bytes>,
    status: StatusCode,
    headers: HeaderMap,
    transform: ResponseTransform,
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamPreflightError {
    Network(String),
    Protocol(String),
    Semantic(String),
}

impl StreamPreflightError {
    pub(crate) fn classification_text(&self) -> String {
        match self {
            Self::Network(message) => format!("upstream network failure: {message}"),
            Self::Protocol(message) | Self::Semantic(message) => {
                format!("upstream semantic failure: {message}")
            }
        }
    }
}

impl fmt::Display for StreamPreflightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(message) => write!(formatter, "stream 输出前上游网络失败：{message}"),
            Self::Protocol(message) => write!(formatter, "stream 输出前上游响应无效：{message}"),
            Self::Semantic(message) => write!(formatter, "stream 输出前上游语义失败：{message}"),
        }
    }
}

impl UpstreamResponse {
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) async fn text(mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.prefetched_body.pop_front() {
            bytes.extend_from_slice(&chunk);
        }
        if let Some(body) = self.buffered_body.take() {
            bytes.extend_from_slice(&body);
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        let tail = self
            .response
            .take()
            .ok_or_else(|| "upstream response body is unavailable".to_string())?
            .text()
            .await
            .map_err(|error| error.to_string())?;
        bytes.extend_from_slice(tail.as_bytes());
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) async fn preflight_stream_failure(
        &mut self,
        hold_reasoning_for_fallback: bool,
    ) -> Result<(), StreamPreflightError> {
        if !self.status.is_success() {
            return Ok(());
        }
        if let Some(body) = self
            .buffered_body
            .as_deref()
            .filter(|body| looks_like_sse(body))
        {
            return preflight_buffered_sse(body, hold_reasoning_for_fallback);
        }
        if !is_event_stream(&self.headers) || self.response.is_none() {
            return Ok(());
        }
        let mut inspection = Vec::new();
        let mut prefetched_bytes = 0_usize;
        loop {
            let next_chunk = self
                .response
                .as_mut()
                .expect("response checked above")
                .chunk();
            let chunk = tokio::time::timeout(STREAM_PREFLIGHT_IDLE_TIMEOUT, next_chunk)
                .await
                .map_err(|_| {
                    StreamPreflightError::Network("等待上游 SSE 后续事件超时".to_string())
                })?
                .map_err(|error| {
                    StreamPreflightError::Network(format!("读取上游 SSE 首帧失败: {error}"))
                })?;
            let Some(chunk) = chunk else {
                return Err(StreamPreflightError::Protocol(
                    "上游 SSE 在输出内容前结束".to_string(),
                ));
            };
            prefetched_bytes = prefetched_bytes.saturating_add(chunk.len());
            inspection.extend_from_slice(&chunk);
            self.prefetched_body.push_back(chunk);
            let mut parsed_any = false;
            while let Some(index) = next_sse_block_index(&inspection) {
                parsed_any = true;
                let block = String::from_utf8_lossy(&inspection[..index]).into_owned();
                let drain_to = if inspection[index..].starts_with(b"\r\n\r\n") {
                    index + 4
                } else {
                    index + 2
                };
                inspection.drain(..drain_to);
                match preflight_sse_block(&block) {
                    StreamPreflight::Continue => {}
                    StreamPreflight::Reasoning if hold_reasoning_for_fallback => {}
                    StreamPreflight::Reasoning => return Ok(()),
                    StreamPreflight::OutputStarted | StreamPreflight::Terminal => return Ok(()),
                    StreamPreflight::Failure(message) => {
                        return Err(StreamPreflightError::Semantic(message));
                    }
                }
            }
            if prefetched_bytes >= MAX_STREAM_PREFLIGHT_BYTES
                || inspection.len() >= 64 * 1024
                || (!parsed_any && inspection.len() >= 16 * 1024)
            {
                return Ok(());
            }
        }
    }
}

fn preflight_buffered_sse(
    body: &[u8],
    hold_reasoning_for_fallback: bool,
) -> Result<(), StreamPreflightError> {
    let mut inspection = body.to_vec();
    while let Some(index) = next_sse_block_index(&inspection) {
        let block = String::from_utf8_lossy(&inspection[..index]).into_owned();
        let drain_to = if inspection[index..].starts_with(b"\r\n\r\n") {
            index + 4
        } else {
            index + 2
        };
        inspection.drain(..drain_to);
        match preflight_sse_block(&block) {
            StreamPreflight::Continue => {}
            StreamPreflight::Reasoning | StreamPreflight::OutputStarted
                if hold_reasoning_for_fallback => {}
            StreamPreflight::Reasoning | StreamPreflight::OutputStarted => return Ok(()),
            StreamPreflight::Terminal => return Ok(()),
            StreamPreflight::Failure(message) => {
                return Err(StreamPreflightError::Semantic(message));
            }
        }
    }
    Err(StreamPreflightError::Protocol(
        "上游 SSE 在输出内容前结束".to_string(),
    ))
}

enum StreamPreflight {
    Continue,
    Reasoning,
    OutputStarted,
    Terminal,
    Failure(String),
}

fn preflight_sse_block(block: &str) -> StreamPreflight {
    let data = block
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return StreamPreflight::Continue;
    }
    if data == "[DONE]" {
        return StreamPreflight::Terminal;
    }
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return StreamPreflight::Continue;
    };
    if let Some(message) = semantic_failure_message(&value) {
        return StreamPreflight::Failure(message);
    }
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(event_type, "response.completed" | "response.incomplete") {
        return StreamPreflight::Terminal;
    }
    let reasoning_event = event_type.contains("reasoning")
        || value
            .pointer("/item/type")
            .and_then(Value::as_str)
            .is_some_and(|item_type| item_type == "reasoning");
    if reasoning_event {
        return StreamPreflight::Reasoning;
    }
    if event_type.contains("output_item")
        || event_type.ends_with(".delta")
        || event_type.contains("content_part")
        || value.pointer("/choices/0/delta/content").is_some()
        || value.pointer("/choices/0/delta/tool_calls").is_some()
    {
        return StreamPreflight::OutputStarted;
    }
    StreamPreflight::Continue
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTransform {
    None,
    ChatCompletionsToResponses,
    OfficialCodexStreamToResponse,
}

pub(crate) struct UpstreamRequest<'a> {
    provider: &'a ProviderConfig,
    method: &'a Method,
    uri: &'a Uri,
    headers: &'a HeaderMap,
    body: Bytes,
    upstream: &'a str,
}

impl<'a> UpstreamRequest<'a> {
    pub(crate) fn new(
        provider: &'a ProviderConfig,
        method: &'a Method,
        uri: &'a Uri,
        headers: &'a HeaderMap,
        body: Bytes,
        upstream: &'a str,
    ) -> Self {
        Self {
            provider,
            method,
            uri,
            headers,
            body,
            upstream,
        }
    }
}

#[derive(Clone, Copy)]
struct UpstreamRequestHeaders<'a> {
    official_codex: bool,
    authorization: Option<&'a str>,
    chatgpt_account_id: Option<&'a str>,
    session_identity: Option<&'a str>,
}

pub(crate) async fn send_upstream(
    client: &reqwest::Client,
    api_service: &ApiServiceStore,
    request: UpstreamRequest<'_>,
) -> std::result::Result<UpstreamResponse, String> {
    let UpstreamRequest {
        provider,
        method,
        uri,
        headers,
        body,
        upstream,
    } = request;
    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|error| format!("invalid method: {error}"))?;
    let transform = match response_transform(provider, method, uri) {
        ResponseTransform::None
            if official_codex_needs_non_stream_bridge(provider, method, uri, &body) =>
        {
            ResponseTransform::OfficialCodexStreamToResponse
        }
        transform => transform,
    };
    let upstream = if transform == ResponseTransform::ChatCompletionsToResponses {
        chat_completions_url(provider, upstream)
    } else {
        upstream.to_string()
    };
    let mut authorization = None;
    let mut agent_identity_task_id = None;
    let mut chatgpt_account_id = None;
    if provider.kind == ProviderKind::OfficialCodex {
        chatgpt_account_id = provider
            .account
            .as_ref()
            .and_then(|account| account.account_id.clone())
            .filter(|account_id| !account_id.trim().is_empty());
        if provider_uses_agent_identity(provider) {
            let auth = ensure_agent_identity_authorization(client, provider, None)
                .await
                .map_err(|error| error.to_string())?;
            agent_identity_task_id = Some(auth.task_id);
            authorization = Some(auth.header);
        } else {
            let auth = ensure_codex_auth_snapshot(provider)
                .await
                .map_err(|error| error.to_string())?;
            chatgpt_account_id = chatgpt_account_id.or(auth.account_id);
            authorization = Some(format!("Bearer {}", auth.access_token));
        }
    } else if let Some(token) = resolve_auth_token(provider) {
        authorization = Some(format!("Bearer {token}"));
    }
    let session_identity = official_codex_session_identity(provider, method, uri, headers, &body);
    let body = normalize_official_responses_input(
        provider,
        method,
        uri,
        body,
        session_identity.as_deref(),
    );
    let body = rewrite_model(provider, body);
    let body = normalize_ultra_reasoning_effort(body);
    let (body, tool_context, chat_messages) =
        if transform == ResponseTransform::ChatCompletionsToResponses {
            responses_body_to_chat_completions_with_store(
                body,
                &provider.id,
                provider_supports_chat_prompt_cache_key(provider),
                Some(api_service),
            )
        } else {
            (body, ChatToolContext::default(), Vec::new())
        };
    let mut response = build_upstream_request(
        client,
        &reqwest_method,
        &upstream,
        headers,
        UpstreamRequestHeaders {
            official_codex: provider.kind == ProviderKind::OfficialCodex,
            authorization: authorization.as_deref(),
            chatgpt_account_id: chatgpt_account_id.as_deref(),
            session_identity: session_identity.as_deref(),
        },
    )
    .body(body.clone())
    .send()
    .await
    .map_err(|error| format_upstream_request_error(&error, &upstream))?;
    let mut buffered_error = None;
    if agent_identity_task_id.is_some() && response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let task_id = agent_identity_task_id
            .as_deref()
            .expect("agent identity task checked above");
        let status = response.status();
        let response_headers = response.headers().clone();
        let error_body = response
            .bytes()
            .await
            .map_err(|error| format!("读取 Agent Identity 失败响应失败: {error}"))?;
        let error_text = String::from_utf8_lossy(&error_body).into_owned();
        if is_agent_identity_task_invalid(status, &error_text) {
            let auth = ensure_agent_identity_authorization(client, provider, Some(task_id))
                .await
                .map_err(|error| error.to_string())?;
            response = build_upstream_request(
                client,
                &reqwest_method,
                &upstream,
                headers,
                UpstreamRequestHeaders {
                    official_codex: true,
                    authorization: Some(&auth.header),
                    chatgpt_account_id: chatgpt_account_id.as_deref(),
                    session_identity: session_identity.as_deref(),
                },
            )
            .body(body.clone())
            .send()
            .await
            .map_err(|error| format_upstream_request_error(&error, &upstream))?;
        } else {
            let redacted = redact_agent_identity_body(provider, &error_text);
            buffered_error = Some(Bytes::from(redacted));
            return Ok(UpstreamResponse {
                response: None,
                buffered_body: buffered_error,
                prefetched_body: VecDeque::new(),
                status,
                headers: response_headers,
                transform,
                tool_context,
                chat_messages,
            });
        }
    }
    let status = response.status();
    let headers = response.headers().clone();
    let inspect_body = status.is_success()
        && method == Method::POST
        && uri
            .path_and_query()
            .is_some_and(|value| is_responses_url(value.as_str()))
        && !is_event_stream(&headers);
    let (response, buffered_body) = if inspect_body {
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("读取上游成功响应失败: {error}"))?;
        if !looks_like_sse(&body) {
            if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                if let Some(message) = semantic_failure_message(&value) {
                    return Err(format!("upstream semantic failure: {message}"));
                }
            }
        }
        (None, Some(body))
    } else {
        (Some(response), buffered_error)
    };
    Ok(UpstreamResponse {
        response,
        buffered_body,
        prefetched_body: VecDeque::new(),
        status,
        headers,
        transform,
        tool_context,
        chat_messages,
    })
}

fn format_upstream_request_error(error: &reqwest::Error, upstream: &str) -> String {
    let endpoint = reqwest::Url::parse(upstream)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "上游服务".to_string());
    if error.is_timeout() {
        return format!("上游网络连接超时（{endpoint}）");
    }
    if error.is_connect() {
        return format!("上游网络连接失败（{endpoint}）");
    }
    format!("上游网络请求失败（{endpoint}）")
}

fn build_upstream_request(
    client: &reqwest::Client,
    method: &reqwest::Method,
    upstream: &str,
    headers: &HeaderMap,
    request_headers: UpstreamRequestHeaders<'_>,
) -> reqwest::RequestBuilder {
    let UpstreamRequestHeaders {
        official_codex,
        authorization,
        chatgpt_account_id,
        session_identity,
    } = request_headers;
    let mut request = client.request(method.clone(), upstream);
    for (name, value) in headers {
        // Accept-Encoding 必须剥掉：relay 未启用响应解压，压缩响应会破坏 SSE 预检和协议转换。
        // x-api-key 是 relay 自己的 client 密钥，不能泄露给上游。
        if matches!(
            *name,
            header::HOST | header::AUTHORIZATION | header::CONTENT_LENGTH | header::ACCEPT_ENCODING
        ) || name.as_str() == "x-api-key"
        {
            continue;
        }
        request = request.header(name, value);
    }
    if let Some(authorization) = authorization {
        request = request.header(header::AUTHORIZATION, authorization);
    }
    if let Some(account_id) = chatgpt_account_id {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    if let Some(session_identity) = session_identity {
        request = request.header("session_id", session_identity);
    }
    if official_codex {
        request = request
            .header("originator", "codex_cli_rs")
            .header("version", "0.144.1");
    }
    request
}

fn normalize_official_responses_input(
    provider: &ProviderConfig,
    method: &Method,
    uri: &Uri,
    body: Bytes,
    session_identity: Option<&str>,
) -> Bytes {
    if provider.kind != ProviderKind::OfficialCodex
        || method != Method::POST
        || !uri
            .path_and_query()
            .is_some_and(|value| is_responses_url(value.as_str()))
    {
        return body;
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let normalized_ids = normalize_official_input_item_ids(&mut value);
    if !uri
        .path_and_query()
        .is_some_and(|value| is_responses_generation_url(value.as_str()))
    {
        return if normalized_ids {
            serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
        } else {
            body
        };
    }
    value["store"] = Value::Bool(false);
    value["stream"] = Value::Bool(true);
    if let Some(object) = value.as_object_mut() {
        for key in [
            "max_output_tokens",
            "max_completion_tokens",
            "temperature",
            "top_p",
            "truncation",
            "user",
            "context_management",
        ] {
            object.remove(key);
        }
        if object
            .get("service_tier")
            .and_then(Value::as_str)
            .is_some_and(|tier| tier != "priority")
        {
            object.remove("service_tier");
        }
        object
            .entry("parallel_tool_calls")
            .or_insert(Value::Bool(true));
    }
    if value.get("prompt_cache_key").is_none() {
        if let Some(session_identity) = session_identity {
            value["prompt_cache_key"] = Value::String(session_identity.to_string());
        }
    }
    match value.get_mut("include") {
        Some(Value::Array(include))
            if !include
                .iter()
                .any(|item| item.as_str() == Some("reasoning.encrypted_content")) =>
        {
            include.push(Value::String("reasoning.encrypted_content".to_string()));
        }
        Some(Value::Array(_)) => {}
        None => {
            value["include"] = json!(["reasoning.encrypted_content"]);
        }
        _ => {}
    }
    if let Some(input) = value
        .get("input")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        value["input"] = json!([{
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": input
            }]
        }]);
    }
    if let Some(items) = value.get_mut("input").and_then(Value::as_array_mut) {
        for item in items {
            if item.get("role").and_then(Value::as_str) == Some("system") {
                item["role"] = Value::String("developer".to_string());
            }
        }
    }
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

pub(crate) fn normalize_official_input_item_ids(value: &mut Value) -> bool {
    let mut changed = false;
    if let Some(items) = value.get_mut("input").and_then(Value::as_array_mut) {
        for item in items {
            changed |= normalize_official_input_item_id(item);
        }
    }
    if let Some(items) = value
        .pointer_mut("/response/input")
        .and_then(Value::as_array_mut)
    {
        for item in items {
            changed |= normalize_official_input_item_id(item);
        }
    }
    changed
}

fn normalize_official_input_item_id(item: &mut Value) -> bool {
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    let expected_prefix = match object.get("type").and_then(Value::as_str) {
        Some("custom_tool_call") => "ctc_",
        Some("reasoning") => "rs_",
        Some("function_call") => "fc_",
        Some("message") => "msg_",
        _ => return false,
    };
    let Some(id) = object.get("id").and_then(Value::as_str) else {
        return false;
    };
    if id.starts_with(expected_prefix) {
        return false;
    }
    let suffix = id.split_once('_').map_or(id, |(_, suffix)| suffix);
    object.insert(
        "id".to_string(),
        Value::String(format!("{expected_prefix}{suffix}")),
    );
    true
}

fn official_codex_session_identity(
    provider: &ProviderConfig,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Option<String> {
    if provider.kind != ProviderKind::OfficialCodex
        || method != Method::POST
        || !uri
            .path_and_query()
            .is_some_and(|value| is_responses_generation_url(value.as_str()))
    {
        return None;
    }
    for header in ["session_id", "x-session-id", "x-client-request-id"] {
        if let Some(value) = headers
            .get(header)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let identity = [
        value.get("prompt_cache_key"),
        value.get("session_id"),
        value.pointer("/metadata/session_id"),
        value.get("conversation_id"),
        value.get("thread_id"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::trim)
    .find(|value| !value.is_empty())
    .map(ToOwned::to_owned);
    identity
}

fn official_codex_needs_non_stream_bridge(
    provider: &ProviderConfig,
    method: &Method,
    uri: &Uri,
    body: &[u8],
) -> bool {
    if provider.kind != ProviderKind::OfficialCodex
        || method != Method::POST
        || !uri
            .path_and_query()
            .is_some_and(|value| is_responses_generation_url(value.as_str()))
    {
        return false;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        != Some(true)
}

fn rewrite_model(provider: &ProviderConfig, body: Bytes) -> Bytes {
    if provider.model_map.is_empty() {
        return body;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(model) = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
    else {
        return body;
    };
    let Some(mapped) = provider.model_map.get(&model).cloned() else {
        return body;
    };
    if mapped == model {
        return body;
    }
    value["model"] = serde_json::Value::String(mapped);
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

fn normalize_ultra_reasoning_effort(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let mut changed = false;
    if let Some(effort) = value
        .get_mut("reasoning")
        .and_then(Value::as_object_mut)
        .and_then(|reasoning| reasoning.get_mut("effort"))
        .filter(|effort| effort.as_str() == Some("ultra"))
    {
        *effort = Value::String("max".to_string());
        changed = true;
    }
    if let Some(effort) = value
        .get_mut("reasoning_effort")
        .filter(|effort| effort.as_str() == Some("ultra"))
    {
        *effort = Value::String("max".to_string());
        changed = true;
    }
    if changed {
        serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
    } else {
        body
    }
}

pub(crate) async fn stream_response(
    store: ConfigStore,
    request_id: String,
    provider_id: String,
    upstream: UpstreamResponse,
) -> Response {
    let UpstreamResponse {
        response,
        buffered_body,
        prefetched_body,
        status,
        headers,
        transform,
        tool_context,
        chat_messages,
    } = upstream;
    if transform == ResponseTransform::OfficialCodexStreamToResponse && status.is_success() {
        let body = collect_response_bytes(response, buffered_body, prefetched_body).await;
        return match body.and_then(|body| terminal_response_from_sse(&body)) {
            Ok(value) => Response::builder()
                .status(status)
                .header(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json; charset=utf-8"),
                )
                .header("x-codex-companion-provider", provider_id)
                .body(Body::from(value.to_string()))
                .unwrap_or_else(|error| {
                    text_response(
                        StatusCode::BAD_GATEWAY,
                        format!("response build failed: {error}"),
                    )
                }),
            Err(error) => text_response(
                StatusCode::BAD_GATEWAY,
                format!("invalid official Codex stream: {error}"),
            ),
        };
    }
    if transform == ResponseTransform::ChatCompletionsToResponses {
        if is_event_stream(&headers)
            || buffered_body
                .as_ref()
                .is_some_and(|body| looks_like_sse(body))
        {
            return chat_sse_response(
                provider_id,
                status,
                response_byte_stream(response, buffered_body, prefetched_body),
                tool_context,
                chat_messages,
                store,
                request_id,
            );
        }
        let body =
            String::from_utf8_lossy(buffered_body.as_deref().unwrap_or_default()).into_owned();
        let value = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| {
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": body
                    }
                }]
            })
        });
        let converted = chat_json_to_responses_json(value.clone(), &tool_context);
        let api_service = ApiServiceStore::from_config_store(&store);
        store_non_stream_chat_history(
            &provider_id,
            &converted,
            &value,
            &tool_context,
            &chat_messages,
            Some(&api_service),
        );
        let body = converted.to_string();
        return Response::builder()
            .status(status)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            )
            .header("x-codex-companion-provider", provider_id)
            .body(Body::from(body))
            .unwrap_or_else(|error| {
                text_response(
                    StatusCode::BAD_GATEWAY,
                    format!("response build failed: {error}"),
                )
            });
    }

    let observe_responses_stream = is_event_stream(&headers)
        || buffered_body
            .as_ref()
            .is_some_and(|body| looks_like_sse(body));
    let stream = response_byte_stream(response, buffered_body, prefetched_body);
    let stream = if observe_responses_stream {
        responses_sse_observer_stream(
            stream,
            ResponsesSseObserverState::new(store, provider_id.clone(), request_id),
        )
    } else {
        stream
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if matches!(*name, header::CONNECTION | header::TRANSFER_ENCODING) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .header("x-codex-companion-provider", provider_id)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|error| {
            text_response(
                StatusCode::BAD_GATEWAY,
                format!("response build failed: {error}"),
            )
        })
}

async fn collect_response_bytes(
    response: Option<reqwest::Response>,
    buffered_body: Option<Bytes>,
    prefetched_body: VecDeque<Bytes>,
) -> Result<Bytes, String> {
    let mut bytes = Vec::new();
    for chunk in prefetched_body {
        bytes.extend_from_slice(&chunk);
    }
    if let Some(body) = buffered_body {
        bytes.extend_from_slice(&body);
    } else if let Some(response) = response {
        let tail = response.bytes().await.map_err(|error| error.to_string())?;
        bytes.extend_from_slice(&tail);
    }
    Ok(Bytes::from(bytes))
}

fn chat_sse_response(
    provider_id: String,
    status: StatusCode,
    upstream: ResponseByteStream,
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
    store: ConfigStore,
    request_id: String,
) -> Response {
    let state = ChatSseTransformState::new(provider_id.clone(), tool_context, chat_messages)
        .with_observer(store, request_id);
    let stream = chat_sse_to_responses_stream(upstream, state);
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        )
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .header("x-codex-companion-provider", provider_id)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|error| {
            text_response(
                StatusCode::BAD_GATEWAY,
                format!("response build failed: {error}"),
            )
        })
}

fn response_byte_stream(
    response: Option<reqwest::Response>,
    buffered_body: Option<Bytes>,
    prefetched_body: VecDeque<Bytes>,
) -> ResponseByteStream {
    if let Some(body) = buffered_body {
        return Box::pin(
            stream::iter(prefetched_body.into_iter().map(Ok))
                .chain(stream::once(async move { Ok(body) })),
        );
    }
    let prefetched = stream::iter(prefetched_body.into_iter().map(Ok));
    let tail: ResponseByteStream = match response {
        Some(response) => Box::pin(response.bytes_stream().map_err(io::Error::other)),
        None => Box::pin(stream::empty()),
    };
    Box::pin(prefetched.chain(tail))
}

fn responses_sse_observer_stream(
    upstream: ResponseByteStream,
    state: ResponsesSseObserverState,
) -> ResponseByteStream {
    Box::pin(stream::unfold(
        (state, upstream),
        |(mut state, mut upstream)| async move {
            if let Some(bytes) = state.pending.pop_front() {
                return Some((Ok(bytes), (state, upstream)));
            }
            match upstream.next().await {
                Some(Ok(chunk)) => {
                    state.push_chunk(&chunk);
                    Some((Ok(chunk), (state, upstream)))
                }
                Some(Err(error)) => {
                    state.fail_incomplete(&format!("上游流读取失败: {error}"));
                    state
                        .pending
                        .pop_front()
                        .map(|bytes| (Ok(bytes), (state, upstream)))
                }
                None => {
                    state.finish_stream();
                    state
                        .pending
                        .pop_front()
                        .map(|bytes| (Ok(bytes), (state, upstream)))
                }
            }
        },
    ))
}

#[derive(Debug)]
struct ResponsesSseObserverState {
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    terminal: bool,
    failure_recorded: bool,
    response_id: String,
    model: String,
    store: ConfigStore,
    api_service: ApiServiceStore,
    provider_id: String,
    request_id: String,
}

impl ResponsesSseObserverState {
    fn new(store: ConfigStore, provider_id: String, request_id: String) -> Self {
        Self {
            buffer: Vec::new(),
            pending: VecDeque::new(),
            terminal: false,
            failure_recorded: false,
            response_id: "resp_codex_companion".to_string(),
            model: String::new(),
            api_service: ApiServiceStore::from_config_store(&store),
            store,
            provider_id,
            request_id,
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some(index) = next_sse_block_index(&self.buffer) {
            let block = String::from_utf8_lossy(&self.buffer[..index]).into_owned();
            let drain_to = if self.buffer[index..].starts_with(b"\r\n\r\n") {
                index + 4
            } else {
                index + 2
            };
            self.buffer.drain(..drain_to);
            self.process_block(&block);
        }
    }

    fn process_block(&mut self, block: &str) {
        let data = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        if let Some(id) = value
            .pointer("/response/id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.response_id = id.to_string();
        }
        if let Some(model) = value
            .pointer("/response/model")
            .or_else(|| value.get("model"))
            .and_then(Value::as_str)
        {
            self.model = model.to_string();
        }
        match value.get("type").and_then(Value::as_str) {
            Some("response.completed" | "response.incomplete") => self.terminal = true,
            Some("response.failed" | "error") => {
                self.terminal = true;
                self.record_failure(&compact_json_error(&value));
            }
            _ => {}
        }
    }

    fn finish_stream(&mut self) {
        if !self.buffer.is_empty() {
            let block = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            self.process_block(&block);
        }
        if !self.terminal {
            self.fail_incomplete("上游流在完成事件前中断");
        }
    }

    fn fail_incomplete(&mut self, detail: &str) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        self.record_failure(detail);
        let event = json!({
            "type": "response.failed",
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "failed",
                "model": self.model,
                "output": [],
                "error": {
                    "code": "upstream_stream_incomplete",
                    "message": detail
                }
            }
        });
        self.pending
            .push_back(Bytes::from(format!("data: {event}\n\n")));
    }

    fn record_failure(&mut self, detail: &str) {
        if self.failure_recorded {
            return;
        }
        self.failure_recorded = true;
        let detail = detail.trim();
        let message = format!("[{}] upstream_stream_incomplete: {detail}", self.request_id);
        let failure = classify_failure(None, &message);
        crate::events::update_health(&self.store, &self.provider_id, |health| {
            mark_failure(health, &failure, message.clone())
        });
        crate::events::append_event(
            &self.store,
            "error",
            Some(self.provider_id.clone()),
            message,
        );
        let _ = self.api_service.record_stream_outcome(
            &self.request_id,
            "failed",
            Some(if detail.is_empty() {
                "upstream_stream_failed"
            } else {
                detail
            }),
        );
    }
}

fn next_sse_block_index(buffer: &[u8]) -> Option<usize> {
    match (find_bytes(buffer, b"\n\n"), find_bytes(buffer, b"\r\n\r\n")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

fn looks_like_sse(body: &[u8]) -> bool {
    body.iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .take(5)
        .eq(b"data:".iter().copied())
}

fn terminal_response_from_sse(body: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(body).map_err(|error| format!("invalid UTF-8: {error}"))?;
    let normalized = text.replace("\r\n", "\n");
    let mut failure = None;
    let mut output_items = BTreeMap::<u64, Value>::new();
    for event in normalized.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                if let (Some(index), Some(item)) = (
                    value.get("output_index").and_then(Value::as_u64),
                    value.get("item").cloned(),
                ) {
                    output_items.insert(index, item);
                }
            }
            Some("response.completed" | "response.incomplete") => {
                let mut response = value
                    .get("response")
                    .cloned()
                    .ok_or_else(|| "terminal event did not include a response".to_string())?;
                if response
                    .get("output")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
                    && !output_items.is_empty()
                {
                    response["output"] = Value::Array(output_items.into_values().collect());
                }
                return Ok(response);
            }
            Some("response.failed" | "error") => {
                failure = Some(compact_json_error(&value));
            }
            _ => {}
        }
    }
    Err(failure.unwrap_or_else(|| "stream ended without a terminal response event".to_string()))
}

fn semantic_failure_message(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if let Some(error) = object.get("error").filter(|error| !error.is_null()) {
        return Some(compact_json_error(error));
    }
    if object
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "failed")
    {
        return Some(compact_json_error(value));
    }
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "error" | "response.failed"))
    {
        return Some(compact_json_error(value));
    }
    None
}

fn compact_json_error(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| {
            value
                .pointer("/response/error/message")
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn response_transform(provider: &ProviderConfig, method: &Method, uri: &Uri) -> ResponseTransform {
    if method == Method::POST
        && uri
            .path_and_query()
            .is_some_and(|value| is_responses_generation_url(value.as_str()))
        && provider_endpoint_is_chat_completions(&provider.base_url)
    {
        ResponseTransform::ChatCompletionsToResponses
    } else {
        ResponseTransform::None
    }
}

fn is_responses_url(url: &str) -> bool {
    url.split('?')
        .next()
        .is_some_and(|path| path.ends_with("/responses") || path.ends_with("/responses/compact"))
}

fn is_responses_generation_url(url: &str) -> bool {
    url.split('?')
        .next()
        .is_some_and(|path| path.ends_with("/responses"))
}

fn chat_completions_url(provider: &ProviderConfig, upstream: &str) -> String {
    if provider_base_url_is_endpoint(&provider.base_url) {
        return upstream.to_string();
    }
    let query = upstream
        .split_once('?')
        .map(|(_, query)| query)
        .filter(|query| !query.is_empty());
    let base = provider.base_url.trim_end_matches('/');
    let mut url = if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    };
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn provider_supports_chat_prompt_cache_key(provider: &ProviderConfig) -> bool {
    let Ok(url) = url::Url::parse(provider.base_url.trim()) else {
        return false;
    };
    match url.host_str().map(str::to_ascii_lowercase).as_deref() {
        Some("api.openai.com") => true,
        Some("api.kimi.com") => {
            let path = url.path().trim_end_matches('/');
            path == "/coding" || path.starts_with("/coding/")
        }
        _ => false,
    }
}

const TOOL_SEARCH_PROXY_NAME: &str = "tool_search";
const CUSTOM_TOOL_INPUT_FIELD: &str = "input";
const CHAT_TOOL_NAME_MAX_LEN: usize = 64;
const CHAT_HISTORY_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatToolKind {
    Function,
    Namespace,
    Custom,
    ToolSearch,
}

#[derive(Debug, Clone)]
struct ChatToolSpec {
    kind: ChatToolKind,
    name: String,
    namespace: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ChatToolContext {
    chat_tools: Vec<Value>,
    seen_chat_names: HashSet<String>,
    chat_name_to_spec: HashMap<String, ChatToolSpec>,
    namespace_name_to_chat_name: HashMap<(String, String), String>,
}

impl ChatToolContext {
    fn from_request(value: &Value) -> Self {
        let mut context = Self::default();
        if let Some(tools) = value.get("tools").and_then(Value::as_array) {
            for tool in tools {
                context.add_response_tool(tool);
            }
        }
        collect_additional_tools(value, &mut context);
        if let Some(input) = value.get("input") {
            collect_tool_search_output_tools(input, &mut context);
        }
        context
    }

    fn to_persisted(&self) -> Value {
        let specs = self
            .chat_name_to_spec
            .iter()
            .map(|(chat_name, spec)| {
                json!({
                    "chatName": chat_name,
                    "kind": match spec.kind {
                        ChatToolKind::Function => "function",
                        ChatToolKind::Namespace => "namespace",
                        ChatToolKind::Custom => "custom",
                        ChatToolKind::ToolSearch => "tool_search",
                    },
                    "name": spec.name,
                    "namespace": spec.namespace,
                })
            })
            .collect::<Vec<_>>();
        json!({ "chatTools": self.chat_tools, "specs": specs })
    }

    fn from_persisted(value: &Value) -> Self {
        let mut context = Self::default();
        let tools = value
            .get("chatTools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let specs = value
            .get("specs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for spec in specs {
            let Some(chat_name) = spec.get("chatName").and_then(Value::as_str) else {
                continue;
            };
            let kind = match spec.get("kind").and_then(Value::as_str) {
                Some("custom") => ChatToolKind::Custom,
                Some("namespace") => ChatToolKind::Namespace,
                Some("tool_search") => ChatToolKind::ToolSearch,
                _ => ChatToolKind::Function,
            };
            let name = spec
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(chat_name)
                .to_string();
            let namespace = spec
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::to_string);
            let chat_tool = tools
                .iter()
                .find(|tool| {
                    tool.pointer("/function/name").and_then(Value::as_str) == Some(chat_name)
                })
                .cloned()
                .unwrap_or_else(
                    || json!({ "type": "function", "function": { "name": chat_name } }),
                );
            context.add_chat_tool(
                chat_name.to_string(),
                ChatToolSpec {
                    kind,
                    name,
                    namespace,
                },
                chat_tool,
            );
        }
        context
    }

    fn merge_missing_from(&mut self, previous: &Self) {
        for chat_tool in &previous.chat_tools {
            let Some(chat_name) = chat_tool.pointer("/function/name").and_then(Value::as_str)
            else {
                continue;
            };
            let Some(spec) = previous.lookup(chat_name).cloned() else {
                continue;
            };
            self.add_chat_tool(chat_name.to_string(), spec, chat_tool.clone());
        }
    }

    fn lookup(&self, chat_name: &str) -> Option<&ChatToolSpec> {
        self.chat_name_to_spec.get(chat_name)
    }

    fn chat_name_for_response_function(&self, name: &str, namespace: Option<&str>) -> String {
        let Some(namespace) = namespace.filter(|value| !value.is_empty()) else {
            return name.to_string();
        };
        self.namespace_name_to_chat_name
            .get(&(namespace.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| flatten_namespace_tool_name(namespace, name))
    }

    fn add_chat_tool(&mut self, chat_name: String, spec: ChatToolSpec, chat_tool: Value) {
        if chat_name.trim().is_empty() || !self.seen_chat_names.insert(chat_name.clone()) {
            return;
        }
        if let Some(namespace) = spec.namespace.as_ref() {
            self.namespace_name_to_chat_name
                .insert((namespace.clone(), spec.name.clone()), chat_name.clone());
        }
        self.chat_name_to_spec.insert(chat_name, spec);
        self.chat_tools.push(chat_tool);
    }

    fn add_function_tool(&mut self, tool: &Value, namespace: Option<&str>) {
        let Some(name) = response_tool_name(tool) else {
            return;
        };
        let chat_name = namespace
            .map(|namespace| flatten_namespace_tool_name(namespace, &name))
            .unwrap_or_else(|| name.clone());
        let Some(chat_tool) = response_function_tool_to_chat_tool(tool, &chat_name) else {
            return;
        };
        self.add_chat_tool(
            chat_name,
            ChatToolSpec {
                kind: if namespace.is_some() {
                    ChatToolKind::Namespace
                } else {
                    ChatToolKind::Function
                },
                name,
                namespace: namespace.map(ToOwned::to_owned),
            },
            chat_tool,
        );
    }

    fn add_custom_tool(&mut self, tool: &Value) {
        let Some(name) = response_tool_name(tool) else {
            return;
        };
        let original = serde_json::to_string(tool).unwrap_or_default();
        let description = format!(
            "{}\n\nOriginal tool definition:\n{}",
            tool.get("description")
                .and_then(Value::as_str)
                .unwrap_or("Pass raw string input to the original custom tool."),
            original
        );
        let chat_tool = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        CUSTOM_TOOL_INPUT_FIELD: {
                            "type": "string",
                            "description": "Raw string input for the original custom tool."
                        }
                    },
                    "required": [CUSTOM_TOOL_INPUT_FIELD]
                }
            }
        });
        self.add_chat_tool(
            name.clone(),
            ChatToolSpec {
                kind: ChatToolKind::Custom,
                name,
                namespace: None,
            },
            chat_tool,
        );
    }

    fn add_tool_search(&mut self) {
        self.add_chat_tool(
            TOOL_SEARCH_PROXY_NAME.to_string(),
            ChatToolSpec {
                kind: ChatToolKind::ToolSearch,
                name: TOOL_SEARCH_PROXY_NAME.to_string(),
                namespace: None,
            },
            json!({
                "type": "function",
                "function": {
                    "name": TOOL_SEARCH_PROXY_NAME,
                    "description": "Search and load Codex tools, plugins, connectors, and MCP namespaces for the current task.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" },
                            "limit": { "type": "integer" }
                        },
                        "required": ["query"]
                    }
                }
            }),
        );
    }

    fn add_namespace_tool(&mut self, tool: &Value) {
        let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
            return;
        };
        let Some(children) = tool
            .get("tools")
            .or_else(|| tool.get("children"))
            .and_then(Value::as_array)
        else {
            return;
        };
        for child in children {
            if child.get("type").and_then(Value::as_str) == Some("function") {
                self.add_function_tool(child, Some(namespace));
            }
        }
    }

    fn add_response_tool(&mut self, tool: &Value) {
        match tool {
            Value::String(name) => self.add_custom_tool(&json!({
                "type": "custom",
                "name": name
            })),
            Value::Object(_) => match tool.get("type").and_then(Value::as_str) {
                Some("function") => self.add_function_tool(tool, None),
                Some("custom") => self.add_custom_tool(tool),
                Some("tool_search") => self.add_tool_search(),
                Some("namespace") => self.add_namespace_tool(tool),
                _ => {}
            },
            _ => {}
        }
    }
}

fn collect_additional_tools(value: &Value, context: &mut ChatToolContext) {
    match value {
        Value::Object(object) => {
            if let Some(tools) = object.get("additional_tools").and_then(Value::as_array) {
                for tool in tools {
                    context.add_response_tool(tool);
                }
            }
            for key in ["input", "override", "overrides"] {
                if let Some(child) = object.get(key) {
                    collect_additional_tools(child, context);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_additional_tools(item, context);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
struct ChatHistoryEntry {
    messages: Vec<Value>,
    tool_context: ChatToolContext,
}

impl ChatHistoryEntry {
    fn from_persisted(messages: Value, tool_context: Value) -> Option<Self> {
        Some(Self {
            messages: messages.as_array()?.clone(),
            tool_context: ChatToolContext::from_persisted(&tool_context),
        })
    }
}

#[derive(Debug, Default)]
struct ChatHistoryStore {
    entries: HashMap<String, ChatHistoryEntry>,
    order: VecDeque<String>,
}

static CHAT_HISTORY: OnceLock<Mutex<ChatHistoryStore>> = OnceLock::new();

fn chat_history_key(provider_id: &str, response_id: &str) -> String {
    format!("{provider_id}:{response_id}")
}

fn load_chat_history(
    provider_id: &str,
    response_id: &str,
    persistent: Option<&ApiServiceStore>,
) -> Option<ChatHistoryEntry> {
    let store = CHAT_HISTORY.get_or_init(|| Mutex::new(ChatHistoryStore::default()));
    let cached = store
        .lock()
        .ok()?
        .entries
        .get(&chat_history_key(provider_id, response_id))
        .cloned();
    if cached.is_some() {
        return cached;
    }
    let (messages, tool_context) = persistent?
        .load_chat_history(provider_id, response_id)
        .ok()
        .flatten()?;
    let entry = ChatHistoryEntry::from_persisted(messages, tool_context)?;
    cache_chat_history(provider_id, response_id, entry.clone());
    Some(entry)
}

fn store_chat_history(
    provider_id: &str,
    response_id: &str,
    entry: ChatHistoryEntry,
    persistent: Option<&ApiServiceStore>,
) {
    if response_id.trim().is_empty() {
        return;
    }
    if let Some(persistent) = persistent {
        let _ = persistent.store_chat_history(
            provider_id,
            response_id,
            &Value::Array(entry.messages.clone()),
            &entry.tool_context.to_persisted(),
        );
    }
    cache_chat_history(provider_id, response_id, entry);
}

fn cache_chat_history(provider_id: &str, response_id: &str, entry: ChatHistoryEntry) {
    let store = CHAT_HISTORY.get_or_init(|| Mutex::new(ChatHistoryStore::default()));
    let Ok(mut store) = store.lock() else {
        return;
    };
    let key = chat_history_key(provider_id, response_id);
    if !store.entries.contains_key(&key) {
        store.order.push_back(key.clone());
    }
    store.entries.insert(key, entry);
    while store.entries.len() > CHAT_HISTORY_CAPACITY {
        let Some(oldest) = store.order.pop_front() else {
            break;
        };
        store.entries.remove(&oldest);
    }
}

#[cfg(test)]
fn responses_body_to_chat_completions(
    body: Bytes,
    provider_id: &str,
    prompt_cache_enabled: bool,
) -> (Bytes, ChatToolContext, Vec<Value>) {
    responses_body_to_chat_completions_with_store(body, provider_id, prompt_cache_enabled, None)
}

fn responses_body_to_chat_completions_with_store(
    body: Bytes,
    provider_id: &str,
    prompt_cache_enabled: bool,
    persistent: Option<&ApiServiceStore>,
) -> (Bytes, ChatToolContext, Vec<Value>) {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return (body, ChatToolContext::default(), Vec::new());
    };
    let mut tool_context = ChatToolContext::from_request(&value);
    let Some(object) = value.as_object() else {
        return (body, tool_context, Vec::new());
    };
    let previous = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .and_then(|response_id| load_chat_history(provider_id, response_id, persistent));
    if let Some(previous) = previous.as_ref() {
        tool_context.merge_missing_from(&previous.tool_context);
    }

    let mut output = serde_json::Map::new();
    copy_json_field(object, &mut output, "model");
    copy_json_field(object, &mut output, "temperature");
    copy_json_field(object, &mut output, "top_p");
    copy_json_field(object, &mut output, "presence_penalty");
    copy_json_field(object, &mut output, "frequency_penalty");
    copy_json_field(object, &mut output, "parallel_tool_calls");
    copy_json_field(object, &mut output, "response_format");
    copy_json_field(object, &mut output, "metadata");
    if let Some(reasoning_effort) = object
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .or_else(|| object.get("reasoning_effort"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
    {
        output.insert(
            "reasoning_effort".to_string(),
            Value::String(
                if reasoning_effort == "ultra" {
                    "max"
                } else {
                    reasoning_effort
                }
                .to_string(),
            ),
        );
    }
    if prompt_cache_enabled {
        let prompt_cache_key = object
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .or_else(|| {
                object
                    .get("metadata")
                    .and_then(|metadata| metadata.get("session_id"))
                    .and_then(Value::as_str)
            })
            .or_else(|| object.get("conversation_id").and_then(Value::as_str))
            .or_else(|| object.get("thread_id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(prompt_cache_key) = prompt_cache_key {
            output.insert(
                "prompt_cache_key".to_string(),
                Value::String(prompt_cache_key.to_string()),
            );
        }
    }
    if let Some(max_output_tokens) = object.get("max_output_tokens") {
        output.insert("max_tokens".to_string(), max_output_tokens.clone());
    }
    // Responses API 语义：省略 stream 表示非流式 JSON，不能默认成流式。
    output.insert(
        "stream".to_string(),
        object.get("stream").cloned().unwrap_or(Value::Bool(false)),
    );
    if let Some(stream_options) = object.get("stream_options") {
        output.insert("stream_options".to_string(), stream_options.clone());
    } else if output
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        output.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }

    let mut messages = previous
        .as_ref()
        .map(|entry| entry.messages.clone())
        .unwrap_or_default();
    if messages.is_empty() {
        if let Some(instructions) = object.get("instructions").and_then(value_text) {
            if !instructions.is_empty() {
                messages.push(json!({ "role": "system", "content": instructions }));
            }
        }
    }
    append_response_input_messages(object.get("input"), &mut messages, &tool_context);
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    output.insert("messages".to_string(), Value::Array(messages.clone()));

    if !tool_context.chat_tools.is_empty() {
        output.insert(
            "tools".to_string(),
            Value::Array(tool_context.chat_tools.clone()),
        );
        if let Some(tool_choice) = object.get("tool_choice") {
            output.insert(
                "tool_choice".to_string(),
                response_tool_choice_to_chat(tool_choice, &tool_context),
            );
        }
    } else {
        output.remove("parallel_tool_calls");
    }

    let converted = serde_json::to_vec(&Value::Object(output))
        .map(Bytes::from)
        .unwrap_or(body);
    (converted, tool_context, messages)
}

fn copy_json_field(
    source: &serde_json::Map<String, Value>,
    target: &mut serde_json::Map<String, Value>,
    name: &str,
) {
    if let Some(value) = source.get(name).filter(|value| !value.is_null()) {
        target.insert(name.to_string(), value.clone());
    }
}

fn append_response_input_messages(
    input: Option<&Value>,
    messages: &mut Vec<Value>,
    tool_context: &ChatToolContext,
) {
    let mut pending_tool_calls = Vec::new();
    let mut pending_tool_media = Vec::new();
    let mut pending_reasoning = None;
    let mut last_assistant_index = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"));
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({ "role": "user", "content": text }));
            last_assistant_index = None;
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_response_input_item(
                    item,
                    messages,
                    &mut pending_tool_calls,
                    &mut pending_tool_media,
                    &mut pending_reasoning,
                    &mut last_assistant_index,
                    tool_context,
                );
            }
        }
        Some(Value::Object(_)) => append_response_input_item(
            input.expect("object input"),
            messages,
            &mut pending_tool_calls,
            &mut pending_tool_media,
            &mut pending_reasoning,
            &mut last_assistant_index,
            tool_context,
        ),
        _ => {}
    }
    flush_pending_tool_calls(
        messages,
        &mut pending_tool_calls,
        &mut pending_reasoning,
        &mut last_assistant_index,
    );
    flush_pending_tool_media(messages, &mut pending_tool_media);
    attach_pending_reasoning_to_previous_assistant(
        messages,
        last_assistant_index,
        &mut pending_reasoning,
    );
}

fn append_response_input_item(
    item: &Value,
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    pending_tool_media: &mut Vec<Value>,
    pending_reasoning: &mut Option<String>,
    last_assistant_index: &mut Option<usize>,
    tool_context: &ChatToolContext,
) {
    let Some(object) = item.as_object() else {
        flush_pending_tool_calls(
            messages,
            pending_tool_calls,
            pending_reasoning,
            last_assistant_index,
        );
        flush_pending_tool_media(messages, pending_tool_media);
        attach_pending_reasoning_to_previous_assistant(
            messages,
            *last_assistant_index,
            pending_reasoning,
        );
        if let Some(text) = value_text(item) {
            messages.push(json!({ "role": "user", "content": text }));
            *last_assistant_index = None;
        }
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            flush_pending_tool_media(messages, pending_tool_media);
            let call_id = response_call_id(object);
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let namespace = object.get("namespace").and_then(Value::as_str);
            pending_tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": {
                    "name": tool_context.chat_name_for_response_function(name, namespace),
                    "arguments": json_argument_string(object.get("arguments"))
                }
            }));
        }
        Some("custom_tool_call") => {
            flush_pending_tool_media(messages, pending_tool_media);
            let input = object
                .get("input")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            pending_tool_calls.push(json!({
                "id": response_call_id(object),
                "type": "function",
                "function": {
                    "name": object.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "arguments": serde_json::to_string(&json!({ CUSTOM_TOOL_INPUT_FIELD: input }))
                        .unwrap_or_else(|_| "{}".to_string())
                }
            }));
        }
        Some("tool_search_call") => {
            flush_pending_tool_media(messages, pending_tool_media);
            pending_tool_calls.push(json!({
                "id": response_call_id(object),
                "type": "function",
                "function": {
                    "name": TOOL_SEARCH_PROXY_NAME,
                    "arguments": json_argument_string(object.get("arguments"))
                }
            }));
        }
        Some("reasoning") => {
            append_pending_reasoning(pending_reasoning, response_reasoning_item_text(item));
        }
        Some("message") | None => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            flush_pending_tool_media(messages, pending_tool_media);
            let role = object.get("role").and_then(Value::as_str).unwrap_or("user");
            let role = match role {
                "assistant" => "assistant",
                "system" | "developer" => "system",
                "tool" => "tool",
                _ => "user",
            };
            let content = object
                .get("content")
                .and_then(message_content_text)
                .unwrap_or_default();
            let mut message = json!({ "role": role, "content": content });
            if role == "assistant" {
                append_pending_reasoning(pending_reasoning, response_message_reasoning_text(item));
                attach_pending_reasoning_to_assistant(&mut message, pending_reasoning);
                *last_assistant_index = Some(messages.len());
            } else {
                attach_pending_reasoning_to_previous_assistant(
                    messages,
                    *last_assistant_index,
                    pending_reasoning,
                );
                *last_assistant_index = None;
            }
            messages.push(message);
        }
        Some("function_call_output")
        | Some("custom_tool_call_output")
        | Some("tool_search_output") => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            let output = object
                .get("output")
                .or_else(|| object.get("content"))
                .unwrap_or(item);
            let (content, media) = tool_output_content_and_media(output);
            let mut message = json!({ "role": "tool", "content": content });
            if let Some(call_id) = object.get("call_id").and_then(Value::as_str) {
                message["tool_call_id"] = Value::String(call_id.to_string());
            }
            messages.push(message);
            pending_tool_media.extend(media);
        }
        Some("input_text") | Some("output_text") => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            flush_pending_tool_media(messages, pending_tool_media);
            attach_pending_reasoning_to_previous_assistant(
                messages,
                *last_assistant_index,
                pending_reasoning,
            );
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                messages.push(json!({ "role": "user", "content": text }));
                *last_assistant_index = None;
            }
        }
        _ => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            flush_pending_tool_media(messages, pending_tool_media);
        }
    }
}

fn flush_pending_tool_calls(
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    pending_reasoning: &mut Option<String>,
    last_assistant_index: &mut Option<usize>,
) {
    if pending_tool_calls.is_empty() {
        return;
    }
    let mut message = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": std::mem::take(pending_tool_calls)
    });
    attach_pending_reasoning_to_assistant(&mut message, pending_reasoning);
    *last_assistant_index = Some(messages.len());
    messages.push(message);
}

fn response_reasoning_item_text(value: &Value) -> Option<String> {
    for field in ["reasoning_content", "content", "text", "summary"] {
        if let Some(text) = value.get(field).and_then(reasoning_text) {
            return Some(text);
        }
    }
    None
}

fn response_message_reasoning_text(value: &Value) -> Option<String> {
    for field in ["reasoning_content", "reasoning", "reasoning_details"] {
        if let Some(text) = value.get(field).and_then(reasoning_text) {
            return Some(text);
        }
    }
    None
}

fn reasoning_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(reasoning_text)
                .collect::<Vec<_>>()
                .join("\n\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(object) => {
            for field in ["text", "content", "summary", "summary_text", "parts"] {
                if let Some(text) = object.get(field).and_then(reasoning_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

fn append_pending_reasoning(pending: &mut Option<String>, reasoning: Option<String>) {
    let Some(reasoning) = reasoning
        .as_deref()
        .map(str::trim)
        .filter(|reasoning| !reasoning.is_empty())
    else {
        return;
    };
    match pending {
        Some(existing) if !existing.is_empty() => {
            existing.push_str("\n\n");
            existing.push_str(reasoning);
        }
        _ => *pending = Some(reasoning.to_string()),
    }
}

fn attach_pending_reasoning_to_assistant(
    message: &mut Value,
    pending_reasoning: &mut Option<String>,
) {
    let Some(reasoning) = pending_reasoning.take() else {
        return;
    };
    if let Some(object) = message.as_object_mut() {
        append_reasoning_content(object, &reasoning);
    }
}

fn attach_pending_reasoning_to_previous_assistant(
    messages: &mut [Value],
    last_assistant_index: Option<usize>,
    pending_reasoning: &mut Option<String>,
) {
    let Some(reasoning) = pending_reasoning.take() else {
        return;
    };
    let Some(message) = last_assistant_index.and_then(|index| messages.get_mut(index)) else {
        return;
    };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    if let Some(object) = message.as_object_mut() {
        append_reasoning_content(object, &reasoning);
    }
}

fn append_reasoning_content(object: &mut serde_json::Map<String, Value>, reasoning: &str) {
    let reasoning = reasoning.trim();
    if reasoning.is_empty() {
        return;
    }
    match object.get_mut("reasoning_content") {
        Some(Value::String(existing)) if !existing.trim().is_empty() => {
            existing.push_str("\n\n");
            existing.push_str(reasoning);
        }
        _ => {
            object.insert(
                "reasoning_content".to_string(),
                Value::String(reasoning.to_string()),
            );
        }
    }
}

fn flush_pending_tool_media(messages: &mut Vec<Value>, pending_tool_media: &mut Vec<Value>) {
    if pending_tool_media.is_empty() {
        return;
    }
    let mut content = vec![json!({
        "type": "text",
        "text": "Images returned by tool calls are attached below."
    })];
    content.append(pending_tool_media);
    messages.push(json!({ "role": "user", "content": content }));
}

const TOOL_RESULT_IMAGE_MARKER: &str = "[image attached separately]";
const WHOLE_IMAGE_DATA_URL_MIN_BYTES: usize = 8 * 1024;
const BASE64ISH_MIN_BYTES: usize = 16 * 1024;
const MAX_TOOL_MEDIA_DEPTH: usize = 32;

fn tool_output_content_and_media(value: &Value) -> (String, Vec<Value>) {
    let mut media = Vec::new();
    let mut sanitized = sanitize_tool_output_media(value, &mut media, 0);
    if media.is_empty() {
        return (json_value_string(value), media);
    }
    clamp_residual_tool_media_payloads(&mut sanitized);
    (json_value_string(&sanitized), media)
}

fn sanitize_tool_output_media(value: &Value, media: &mut Vec<Value>, depth: usize) -> Value {
    if depth > MAX_TOOL_MEDIA_DEPTH {
        return value.clone();
    }
    match value {
        Value::String(text) => {
            if let Some(part) = whole_string_tool_image(text) {
                push_tool_image(media, part);
                Value::String(TOOL_RESULT_IMAGE_MARKER.to_string())
            } else if matches!(text.trim().chars().next(), Some('{') | Some('[')) {
                let before = media.len();
                let Some(parsed) = serde_json::from_str::<Value>(text).ok() else {
                    return value.clone();
                };
                let mut sanitized = sanitize_tool_output_media(&parsed, media, depth + 1);
                if media.len() == before {
                    value.clone()
                } else {
                    clamp_residual_tool_media_payloads(&mut sanitized);
                    Value::String(
                        serde_json::to_string(&sanitized).unwrap_or_else(|_| {
                            "[tool output media attached separately]".to_string()
                        }),
                    )
                }
            } else {
                value.clone()
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| sanitize_tool_output_media(item, media, depth + 1))
                .collect(),
        ),
        Value::Object(object) => {
            if let Some(part) = structured_tool_image(value) {
                push_tool_image(media, part);
                return json!({
                    "type": "text",
                    "text": TOOL_RESULT_IMAGE_MARKER
                });
            }
            let mut sanitized = object.clone();
            if let Some(content) = sanitized.get_mut("content") {
                *content = sanitize_tool_output_media(content, media, depth + 1);
            }
            Value::Object(sanitized)
        }
        _ => value.clone(),
    }
}

fn whole_string_tool_image(value: &str) -> Option<Value> {
    let value = value.trim();
    if value.len() < WHOLE_IMAGE_DATA_URL_MIN_BYTES || !is_image_base64_data_url(value) {
        return None;
    }
    Some(chat_image_part(value, None))
}

fn structured_tool_image(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let image_type = object.get("type").and_then(Value::as_str);
    match image_type {
        Some("input_image" | "output_image" | "image_url") => {
            let image_url = object.get("image_url").or_else(|| object.get("imageUrl"))?;
            let (url, nested_detail) = image_url_value(image_url)?;
            let detail = object.get("detail").cloned().or(nested_detail);
            supported_tool_image_url(&url).then(|| chat_image_part(&url, detail))
        }
        Some("image") => typed_tool_image(object),
        None => {
            let image_url = object.get("image_url").or_else(|| object.get("imageUrl"))?;
            let (url, nested_detail) = image_url_value(image_url)?;
            is_image_base64_data_url(&url)
                .then(|| chat_image_part(&url, object.get("detail").cloned().or(nested_detail)))
        }
        _ => None,
    }
}

fn typed_tool_image(object: &serde_json::Map<String, Value>) -> Option<Value> {
    if let Some(source) = object.get("source").and_then(Value::as_object) {
        let mime_type = source
            .get("media_type")
            .or_else(|| source.get("mime_type"))
            .or_else(|| source.get("mimeType"))
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        if !mime_type
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
        {
            return None;
        }
        if let Some(url) = source
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| supported_tool_image_url(url))
        {
            return Some(chat_image_part(url, object.get("detail").cloned()));
        }
        if let Some(data) = source.get("data").and_then(Value::as_str) {
            let url = if is_image_base64_data_url(data) {
                data.to_string()
            } else {
                format!("data:{mime_type};base64,{data}")
            };
            return Some(chat_image_part(&url, object.get("detail").cloned()));
        }
    }

    let mime_type = object
        .get("mimeType")
        .or_else(|| object.get("mime_type"))
        .and_then(Value::as_str)?;
    if !mime_type
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
    {
        return None;
    }
    let data = object.get("data").and_then(Value::as_str)?;
    let url = if is_image_base64_data_url(data) {
        data.to_string()
    } else {
        format!("data:{mime_type};base64,{data}")
    };
    Some(chat_image_part(&url, object.get("detail").cloned()))
}

fn image_url_value(value: &Value) -> Option<(String, Option<Value>)> {
    match value {
        Value::String(url) if !url.trim().is_empty() => Some((url.trim().to_string(), None)),
        Value::Object(object) => object
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.trim().is_empty())
            .map(|url| (url.trim().to_string(), object.get("detail").cloned())),
        _ => None,
    }
}

fn chat_image_part(url: &str, detail: Option<Value>) -> Value {
    let mut image_url = serde_json::Map::new();
    image_url.insert("url".to_string(), Value::String(url.to_string()));
    if let Some(detail) = detail {
        image_url.insert("detail".to_string(), detail);
    }
    json!({
        "type": "image_url",
        "image_url": image_url
    })
}

fn supported_tool_image_url(value: &str) -> bool {
    is_image_base64_data_url(value)
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || value
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
}

fn is_image_base64_data_url(value: &str) -> bool {
    let Some(comma_index) = value.find(',') else {
        return false;
    };
    let header = value[..comma_index].to_ascii_lowercase();
    header.starts_with("data:image/") && header.ends_with(";base64")
}

fn push_tool_image(media: &mut Vec<Value>, part: Value) {
    let url = part.pointer("/image_url/url").and_then(Value::as_str);
    let duplicate = media
        .iter()
        .any(|existing| existing.pointer("/image_url/url").and_then(Value::as_str) == url);
    if !duplicate {
        media.push(part);
    }
}

fn clamp_residual_tool_media_payloads(value: &mut Value) {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            let is_large_data_url = trimmed.len() >= WHOLE_IMAGE_DATA_URL_MIN_BYTES
                && trimmed
                    .get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"));
            let is_large_base64 = trimmed.len() >= BASE64ISH_MIN_BYTES
                && trimmed.bytes().all(|byte| {
                    matches!(
                        byte,
                        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'+' | b'/' | b'='
                    )
                });
            if is_large_data_url || is_large_base64 {
                let byte_len = text.len();
                *text = format!("[omitted {byte_len} bytes of tool media]");
            }
        }
        Value::Array(items) => {
            for item in items {
                clamp_residual_tool_media_payloads(item);
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                clamp_residual_tool_media_payloads(item);
            }
        }
        _ => {}
    }
}

fn response_call_id(object: &serde_json::Map<String, Value>) -> String {
    object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn message_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("input_text"))
                        .or_else(|| part.get("output_text"))
                        .and_then(value_text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(text)
        }
        Value::Object(_) => value
            .get("text")
            .or_else(|| value.get("input_text"))
            .or_else(|| value.get("output_text"))
            .and_then(value_text),
        _ => None,
    }
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_value_string(value: &Value) -> String {
    value_text(value)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_else(|_| String::new()))
}

fn json_argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(arguments)) => arguments.clone(),
        Some(arguments) => serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

fn response_tool_name(tool: &Value) -> Option<String> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn response_function_tool_to_chat_tool(tool: &Value, chat_name: &str) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    if let Some(function) = tool.get("function").and_then(Value::as_object) {
        let mut function = function.clone();
        function.insert("name".to_string(), Value::String(chat_name.to_string()));
        normalize_chat_function_parameters(&mut function);
        return Some(json!({ "type": "function", "function": function }));
    }
    let mut function = json!({
        "name": chat_name,
        "description": tool.get("description").cloned().unwrap_or(Value::Null),
        "parameters": tool
            .get("parameters")
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
    });
    if let Some(strict) = tool.get("strict") {
        function["strict"] = strict.clone();
    }
    if let Some(function) = function.as_object_mut() {
        normalize_chat_function_parameters(function);
    }
    Some(json!({ "type": "function", "function": function }))
}

fn normalize_chat_function_parameters(function: &mut serde_json::Map<String, Value>) {
    let parameters = function
        .entry("parameters".to_string())
        .or_insert_with(|| json!({ "type": "object", "properties": {} }));
    let Some(parameters) = parameters.as_object_mut() else {
        *parameters = json!({ "type": "object", "properties": {} });
        return;
    };
    if parameters.get("type").and_then(Value::as_str) != Some("object") {
        parameters.insert("type".to_string(), Value::String("object".to_string()));
    }
}

fn response_tool_choice_to_chat(tool_choice: &Value, context: &ChatToolContext) -> Value {
    let Some(object) = tool_choice.as_object() else {
        return tool_choice.clone();
    };
    match object.get("type").and_then(Value::as_str) {
        Some("function") => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let namespace = object.get("namespace").and_then(Value::as_str);
            json!({
                "type": "function",
                "function": {
                    "name": context.chat_name_for_response_function(name, namespace)
                }
            })
        }
        Some("custom") => json!({
            "type": "function",
            "function": {
                "name": object.get("name").and_then(Value::as_str).unwrap_or_default()
            }
        }),
        Some("tool_search") => json!({
            "type": "function",
            "function": { "name": TOOL_SEARCH_PROXY_NAME }
        }),
        _ => tool_choice.clone(),
    }
}

fn collect_tool_search_output_tools(value: &Value, context: &mut ChatToolContext) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tool_search_output_tools(item, context);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("tool_search_output") {
                if let Some(tools) = object.get("tools").and_then(Value::as_array) {
                    for tool in tools {
                        context.add_response_tool(tool);
                    }
                }
            }
            for child in object.values() {
                collect_tool_search_output_tools(child, context);
            }
        }
        _ => {}
    }
}

fn flatten_namespace_tool_name(namespace: &str, name: &str) -> String {
    let full_name = format!("{namespace}__{name}");
    if full_name.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full_name;
    }
    let hash = stable_short_hash(&full_name);
    let suffix = format!("__{hash:08x}");
    let max_prefix_len = CHAT_TOOL_NAME_MAX_LEN.saturating_sub(suffix.len());
    let prefix = full_name
        .chars()
        .scan(0usize, |bytes, character| {
            let next = *bytes + character.len_utf8();
            (next <= max_prefix_len).then(|| {
                *bytes = next;
                character
            })
        })
        .collect::<String>();
    format!("{prefix}{suffix}")
}

fn stable_short_hash(value: &str) -> u32 {
    value.bytes().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

fn chat_json_to_responses_json(value: Value, tool_context: &ChatToolContext) -> Value {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let message = choice.and_then(|choice| choice.get("message"));
    let text = message
        .and_then(|message| message.get("content"))
        .and_then(message_content_text)
        .unwrap_or_default();
    let response_id = value
        .get("id")
        .and_then(Value::as_str)
        .map(response_id_from_chat_id)
        .unwrap_or_else(|| stable_chat_response_id(&value));
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_at = value
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(unix_now);
    let usage = chat_usage_to_responses_usage(value.get("usage"));
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({
            "id": "msg_codex_companion",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }));
    }
    if let Some(tool_calls) = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    {
        output.extend(
            tool_calls
                .iter()
                .enumerate()
                .filter_map(|(index, tool_call)| {
                    chat_tool_call_to_response_item(tool_call, index, tool_context)
                }),
        );
    }
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "model": model,
        "output": output,
        "usage": usage
    })
}

fn store_non_stream_chat_history(
    provider_id: &str,
    converted: &Value,
    chat_response: &Value,
    tool_context: &ChatToolContext,
    request_messages: &[Value],
    persistent: Option<&ApiServiceStore>,
) {
    let Some(response_id) = converted.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(message) = chat_response
        .pointer("/choices/0/message")
        .filter(|message| message.is_object())
    else {
        return;
    };
    let mut messages = request_messages.to_vec();
    messages.push(message.clone());
    store_chat_history(
        provider_id,
        response_id,
        ChatHistoryEntry {
            messages,
            tool_context: tool_context.clone(),
        },
        persistent,
    );
}

fn chat_tool_call_to_response_item(
    tool_call: &Value,
    index: usize,
    tool_context: &ChatToolContext,
) -> Option<Value> {
    let function = tool_call.get("function")?;
    let chat_name = function.get("name").and_then(Value::as_str)?;
    if chat_name.is_empty() {
        return None;
    }
    let call_id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("call_{index}"));
    let arguments = json_argument_string(function.get("arguments"));
    Some(response_tool_call_item(
        &call_id,
        chat_name,
        &arguments,
        "completed",
        tool_context,
    ))
}

fn response_tool_call_item(
    call_id: &str,
    chat_name: &str,
    arguments: &str,
    status: &str,
    tool_context: &ChatToolContext,
) -> Value {
    match tool_context.lookup(chat_name) {
        Some(spec) if spec.kind == ChatToolKind::ToolSearch => json!({
            "type": "tool_search_call",
            "call_id": call_id,
            "status": status,
            "execution": "client",
            "arguments": parse_tool_arguments_object(arguments)
        }),
        Some(spec) if spec.kind == ChatToolKind::Custom => json!({
            "id": format!("ctc_{call_id}"),
            "type": "custom_tool_call",
            "status": status,
            "call_id": call_id,
            "name": spec.name,
            "input": custom_tool_input(arguments)
        }),
        Some(spec) => {
            let mut item = json!({
                "id": format!("fc_{call_id}"),
                "type": "function_call",
                "status": status,
                "call_id": call_id,
                "name": spec.name,
                "arguments": arguments
            });
            if let Some(namespace) = spec.namespace.as_ref() {
                item["namespace"] = Value::String(namespace.clone());
            }
            item
        }
        None => json!({
            "id": format!("fc_{call_id}"),
            "type": "function_call",
            "status": status,
            "call_id": call_id,
            "name": chat_name,
            "arguments": arguments
        }),
    }
}

fn custom_tool_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get(CUSTOM_TOOL_INPUT_FIELD)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| arguments.to_string())
}

fn parse_tool_arguments_object(arguments: &str) -> Value {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({ "query": arguments }))
}

fn chat_usage_to_responses_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage else {
        return Value::Null;
    };
    let input = usage
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input + output);
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": total
    })
}

type ResponseByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send + 'static>>;

fn chat_sse_to_responses_stream(
    upstream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
    state: ChatSseTransformState,
) -> ResponseByteStream {
    let upstream = Box::pin(upstream);
    Box::pin(stream::unfold(
        (state, upstream),
        |(mut state, mut upstream)| async move {
            loop {
                if let Some(bytes) = state.pending.pop_front() {
                    return Some((Ok(bytes), (state, upstream)));
                }
                match upstream.next().await {
                    Some(Ok(chunk)) => {
                        state.push_chunk(&chunk);
                    }
                    Some(Err(error)) => {
                        state.fail_incomplete_with_message(&format!("上游流读取失败: {error}"));
                        if let Some(bytes) = state.pending.pop_front() {
                            return Some((Ok(bytes), (state, upstream)));
                        }
                        return None;
                    }
                    None => {
                        state.finish_stream();
                        if let Some(bytes) = state.pending.pop_front() {
                            return Some((Ok(bytes), (state, upstream)));
                        }
                        return None;
                    }
                }
            }
        },
    ))
}

#[derive(Debug)]
struct ChatSseTransformState {
    buffer: Vec<u8>,
    pending: VecDeque<Bytes>,
    response_id: String,
    model: String,
    created_at: i64,
    text: String,
    started: bool,
    completed: bool,
    saw_terminal_signal: bool,
    latest_usage: Value,
    provider_id: String,
    observer: Option<(ConfigStore, ApiServiceStore, String)>,
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
    tools: BTreeMap<usize, ChatToolCallState>,
}

#[derive(Debug, Clone, Default)]
struct ChatToolCallState {
    call_id: String,
    name: String,
    arguments: String,
}

impl Default for ChatSseTransformState {
    fn default() -> Self {
        Self::new(
            "test-provider".to_string(),
            ChatToolContext::default(),
            Vec::new(),
        )
    }
}

impl ChatSseTransformState {
    fn new(provider_id: String, tool_context: ChatToolContext, chat_messages: Vec<Value>) -> Self {
        Self {
            buffer: Vec::new(),
            pending: VecDeque::new(),
            response_id: "resp_codex_companion".to_string(),
            model: String::new(),
            created_at: unix_now(),
            text: String::new(),
            started: false,
            completed: false,
            saw_terminal_signal: false,
            latest_usage: Value::Null,
            provider_id,
            observer: None,
            tool_context,
            chat_messages,
            tools: BTreeMap::new(),
        }
    }

    fn with_observer(mut self, store: ConfigStore, request_id: String) -> Self {
        let api_service = ApiServiceStore::from_config_store(&store);
        self.observer = Some((store, api_service, request_id));
        self
    }

    fn push_chunk(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some(index) = self.next_block_index() {
            let block = String::from_utf8_lossy(&self.buffer[..index]).into_owned();
            let drain_to = if self.buffer[index..].starts_with(b"\r\n\r\n") {
                index + 4
            } else {
                index + 2
            };
            self.buffer.drain(..drain_to);
            self.process_block(&block);
        }
    }

    fn next_block_index(&self) -> Option<usize> {
        match (
            find_bytes(&self.buffer, b"\n\n"),
            find_bytes(&self.buffer, b"\r\n\r\n"),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    fn process_block(&mut self, block: &str) {
        let data = block
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(str::trim_start)
                    .filter(|line| !line.is_empty())
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return;
        }
        if data.trim() == "[DONE]" {
            self.saw_terminal_signal = true;
            self.finish();
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            self.response_id = response_id_from_chat_id(id);
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            self.model = model.to_string();
        }
        if let Some(created) = value.get("created").and_then(Value::as_i64) {
            self.created_at = created;
        }
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            self.latest_usage = chat_usage_to_responses_usage(Some(usage));
        }
        let choice = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first());
        if choice
            .and_then(|choice| choice.get("finish_reason"))
            .is_some_and(|reason| !reason.is_null())
        {
            self.saw_terminal_signal = true;
        }
        if let Some(delta) = choice.and_then(|choice| choice.get("delta")) {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    self.ensure_started();
                    self.text.push_str(text);
                    self.emit(json!({
                        "type": "response.output_text.delta",
                        "output_index": 0,
                        "content_index": 0,
                        "delta": text
                    }));
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    self.append_tool_call_delta(tool_call);
                }
            }
        }
    }

    fn finish_stream(&mut self) {
        if !self.buffer.is_empty() {
            let block = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            self.process_block(&block);
        }
        if self.completed {
            return;
        }
        if self.saw_terminal_signal {
            self.finish();
        } else {
            self.fail_incomplete();
        }
    }

    fn fail_incomplete(&mut self) {
        self.fail_incomplete_with_message("上游流在完成事件前中断");
    }

    fn fail_incomplete_with_message(&mut self, detail: &str) {
        if self.completed {
            return;
        }
        self.ensure_started();
        self.completed = true;
        let mut response = self.response_object("failed");
        response["error"] = json!({
            "code": "upstream_stream_incomplete",
            "message": detail
        });
        self.emit(json!({
            "type": "response.failed",
            "response": response
        }));
        if let Some((store, api_service, request_id)) = self.observer.as_ref() {
            let message = format!("[{request_id}] upstream_stream_incomplete: {detail}");
            let failure = classify_failure(None, &message);
            crate::events::update_health(store, &self.provider_id, |health| {
                mark_failure(health, &failure, message.clone())
            });
            crate::events::append_event(store, "error", Some(self.provider_id.clone()), message);
            let _ = api_service.record_stream_outcome(
                request_id,
                "failed",
                Some("upstream_stream_incomplete"),
            );
        }
    }

    fn append_tool_call_delta(&mut self, tool_call: &Value) {
        let index = tool_call
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(self.tools.len() as u64) as usize;
        let state = self.tools.entry(index).or_default();
        if let Some(call_id) = tool_call.get("id").and_then(Value::as_str) {
            if !call_id.is_empty() {
                state.call_id = call_id.to_string();
            }
        }
        if let Some(function) = tool_call.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    state.name = name.to_string();
                }
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                state.arguments.push_str(arguments);
            }
        }
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        if self.response_id == "resp_codex_companion" {
            self.response_id = stable_response_id(&json!({
                "provider": self.provider_id,
                "model": self.model,
                "messages": self.chat_messages,
            }));
        }
        self.started = true;
        self.emit(json!({
            "type": "response.created",
            "response": self.response_object("in_progress")
        }));
        self.emit(json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": self.message_item(false)
        }));
        self.emit(json!({
            "type": "response.content_part.added",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": []
            }
        }));
    }

    fn finish(&mut self) {
        if self.completed {
            return;
        }
        self.ensure_started();
        self.completed = true;
        self.emit(json!({
            "type": "response.output_text.done",
            "output_index": 0,
            "content_index": 0,
            "text": self.text
        }));
        self.emit(json!({
            "type": "response.content_part.done",
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": self.text,
                "annotations": []
            }
        }));
        self.emit(json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": self.message_item(true)
        }));
        self.emit_completed_tools();
        self.store_history();
        self.emit(json!({
            "type": "response.completed",
            "response": self.response_object("completed")
        }));
        if let Some((_, api_service, request_id)) = self.observer.as_ref() {
            let _ = api_service.record_stream_outcome(request_id, "succeeded", None);
        }
    }

    fn store_history(&self) {
        let mut messages = self.chat_messages.clone();
        let tool_calls = self
            .tools
            .iter()
            .filter_map(|(index, state)| {
                if state.name.trim().is_empty() {
                    return None;
                }
                Some(json!({
                    "id": if state.call_id.is_empty() {
                        format!("call_{index}")
                    } else {
                        state.call_id.clone()
                    },
                    "type": "function",
                    "function": {
                        "name": state.name,
                        "arguments": state.arguments
                    }
                }))
            })
            .collect::<Vec<_>>();
        let content = (!self.text.is_empty()).then(|| Value::String(self.text.clone()));
        let mut assistant = json!({
            "role": "assistant",
            "content": content.unwrap_or(Value::Null)
        });
        if !tool_calls.is_empty() {
            assistant["tool_calls"] = Value::Array(tool_calls);
        }
        messages.push(assistant);
        store_chat_history(
            &self.provider_id,
            &self.response_id,
            ChatHistoryEntry {
                messages,
                tool_context: self.tool_context.clone(),
            },
            self.observer
                .as_ref()
                .map(|(_, api_service, _)| api_service),
        );
    }

    fn emit_completed_tools(&mut self) {
        let tools = self
            .tools
            .iter()
            .filter(|(_, state)| !state.name.trim().is_empty())
            .map(|(index, state)| (*index, state.clone()))
            .collect::<Vec<_>>();
        for (position, (chat_index, state)) in tools.into_iter().enumerate() {
            let output_index = position + 1;
            let call_id = if state.call_id.is_empty() {
                format!("call_{chat_index}")
            } else {
                state.call_id
            };
            let pending_item = response_tool_call_item(
                &call_id,
                &state.name,
                "",
                "in_progress",
                &self.tool_context,
            );
            self.emit(json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": pending_item
            }));

            let completed_item = response_tool_call_item(
                &call_id,
                &state.name,
                &state.arguments,
                "completed",
                &self.tool_context,
            );
            if self
                .tool_context
                .lookup(&state.name)
                .is_some_and(|spec| spec.kind == ChatToolKind::Custom)
            {
                let input = custom_tool_input(&state.arguments);
                if !input.is_empty() {
                    self.emit(json!({
                        "type": "response.custom_tool_call_input.delta",
                        "item_id": format!("ctc_{call_id}"),
                        "output_index": output_index,
                        "delta": input
                    }));
                }
                self.emit(json!({
                    "type": "response.custom_tool_call_input.done",
                    "item_id": format!("ctc_{call_id}"),
                    "output_index": output_index,
                    "input": custom_tool_input(&state.arguments)
                }));
            } else {
                self.emit(json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": format!("fc_{call_id}"),
                    "output_index": output_index,
                    "delta": state.arguments
                }));
                self.emit(json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": format!("fc_{call_id}"),
                    "output_index": output_index,
                    "arguments": state.arguments
                }));
            }
            self.emit(json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": completed_item
            }));
        }
    }

    fn emit(&mut self, value: Value) {
        self.pending
            .push_back(Bytes::from(format!("data: {value}\n\n")));
    }

    fn response_object(&self, status: &str) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": if status == "completed" {
                Value::Array(self.completed_output_items())
            } else {
                json!([])
            },
            "usage": if status == "completed" {
                self.latest_usage.clone()
            } else {
                Value::Null
            }
        })
    }

    fn completed_output_items(&self) -> Vec<Value> {
        let mut output = vec![self.message_item(true)];
        output.extend(self.tools.iter().filter_map(|(index, state)| {
            if state.name.trim().is_empty() {
                return None;
            }
            let call_id = if state.call_id.is_empty() {
                format!("call_{index}")
            } else {
                state.call_id.clone()
            };
            Some(response_tool_call_item(
                &call_id,
                &state.name,
                &state.arguments,
                "completed",
                &self.tool_context,
            ))
        }));
        output
    }

    fn message_item(&self, completed: bool) -> Value {
        json!({
            "id": "msg_codex_companion",
            "type": "message",
            "status": if completed { "completed" } else { "in_progress" },
            "role": "assistant",
            "content": if completed {
                json!([{
                    "type": "output_text",
                    "text": self.text,
                    "annotations": []
                }])
            } else {
                json!([])
            }
        })
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn response_id_from_chat_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else {
        format!("resp_{id}")
    }
}

fn stable_response_id(value: &Value) -> String {
    let serialized = serde_json::to_vec(value).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(serialized));
    format!("resp_cc_{}", &digest[..24])
}

fn stable_chat_response_id(value: &Value) -> String {
    stable_response_id(&json!({
        "model": value.get("model"),
        "choices": value.get("choices"),
    }))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn text_response(status: StatusCode, text: impl Into<String>) -> Response {
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(text.into()))
        .expect("response")
}

pub(crate) fn upstream_url(provider: &ProviderConfig, uri: &Uri) -> String {
    if provider_base_url_is_endpoint(&provider.base_url) {
        if uri.path().ends_with("/responses/compact") {
            let (endpoint, query) = provider
                .base_url
                .trim()
                .split_once('?')
                .map(|(endpoint, query)| (endpoint, Some(query)))
                .unwrap_or((provider.base_url.trim(), None));
            let endpoint = endpoint.trim_end_matches('/');
            if endpoint.ends_with("/responses") {
                let mut compact_endpoint = format!("{endpoint}/compact");
                if let Some(query) = query.filter(|query| !query.is_empty()) {
                    compact_endpoint.push('?');
                    compact_endpoint.push_str(query);
                }
                return compact_endpoint;
            }
        }
        return provider.base_url.trim().to_string();
    }

    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let upstream_path = if path == "/v1" {
        "/".to_string()
    } else {
        path.strip_prefix("/v1/")
            .map(|value| {
                let mut output = String::from("/");
                output.push_str(value);
                output
            })
            .unwrap_or_else(|| path.to_string())
    };
    let mut url = format!(
        "{}{}",
        provider.base_url.trim_end_matches('/'),
        upstream_path
    );
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    if provider.kind == ProviderKind::OfficialCodex {
        url = append_client_version(url);
    }
    url
}

fn append_client_version(mut url: String) -> String {
    if url.contains("client_version=") {
        return url;
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    url.push(separator);
    url.push_str("client_version=");
    url.push_str(env!("CARGO_PKG_VERSION"));
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{default_refresh_interval_seconds, HealthFailureKind, ProviderKind};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn upstream_connection_error_has_a_user_facing_network_message() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        drop(listener);
        let upstream = format!("http://{address}/v1/responses");
        let error = reqwest::Client::new()
            .post(&upstream)
            .send()
            .await
            .expect_err("closed port should reject the connection");

        let message = format_upstream_request_error(&error, &upstream);

        assert!(message.starts_with("上游网络连接失败"));
        assert!(message.contains("127.0.0.1"));
        assert!(!message.contains("error sending request"));
    }

    fn official_provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            id: "official".to_string(),
            name: "Official".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: base_url.to_string(),
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
    fn builds_upstream_url_from_v1_base() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/chat/completions?stream=true".parse().expect("uri");
        assert_eq!(
            upstream_url(&provider, &uri),
            "https://api.example.com/v1/chat/completions?stream=true"
        );
    }

    #[test]
    fn upstream_request_strips_encoding_and_relay_credential_headers() {
        let client = reqwest::Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:1455"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer local-relay-key"),
        );
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br"),
        );
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        headers.insert("x-api-key", HeaderValue::from_static("relay-client-secret"));
        headers.insert("x-session-id", HeaderValue::from_static("thread-1"));

        let request = build_upstream_request(
            &client,
            &reqwest::Method::POST,
            "https://api.example.com/v1/responses",
            &headers,
            UpstreamRequestHeaders {
                official_codex: false,
                authorization: Some("Bearer upstream-token"),
                chatgpt_account_id: None,
                session_identity: None,
            },
        )
        .build()
        .expect("request");

        let sent = request.headers();
        // 透传 Accept-Encoding 会让上游返回 gzip，relay 不解压时 SSE 预检和
        // 协议转换读到的是压缩字节流(乱码)。
        assert!(!sent.contains_key(header::ACCEPT_ENCODING));
        assert!(!sent.contains_key("x-api-key"));
        assert!(!sent.contains_key(header::HOST));
        assert!(!sent.contains_key(header::CONTENT_LENGTH));
        assert_eq!(
            sent.get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer upstream-token")
        );
        assert_eq!(
            sent.get("x-session-id")
                .and_then(|value| value.to_str().ok()),
            Some("thread-1")
        );
    }

    #[test]
    fn semantic_failure_detection_does_not_reject_valid_incomplete_response() {
        assert!(semantic_failure_message(&json!({
            "status": "failed",
            "error": {"message": "overloaded"}
        }))
        .is_some());
        assert!(semantic_failure_message(&json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type": "message"}]
        }))
        .is_none());
    }

    #[test]
    fn rewrites_request_model_from_provider_map() {
        let mut model_map = BTreeMap::new();
        model_map.insert("gpt-5-codex".to_string(), "deepseek-chat".to_string());
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map,
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let body = rewrite_model(
            &provider,
            Bytes::from_static(
                br#"{"model":"gpt-5-codex","input":"hello","reasoning":{"effort":"ultra"}}"#,
            ),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["model"], "deepseek-chat");
        assert_eq!(value["reasoning"]["effort"], "ultra");
    }

    #[test]
    fn normalizes_ultra_to_official_upstream_max_effort() {
        let body = normalize_ultra_reasoning_effort(Bytes::from_static(
            br#"{"reasoning":{"effort":"ultra"},"reasoning_effort":"ultra"}"#,
        ));
        let value: Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(value["reasoning"]["effort"], "max");
        assert_eq!(value["reasoning_effort"], "max");
    }

    #[test]
    fn detects_semantic_failure_in_buffered_sse() {
        let error = preflight_buffered_sse(
            concat!(
                "data: {\"type\":\"response.created\"}\n\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\"}}\n\n",
                "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checking\"}\n\n",
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":",
                "\"exceeded retry limit, last status: 429 Too Many Requests\"}}}\n\n"
            )
            .as_bytes(),
            true,
        )
        .expect_err("semantic failure");

        assert!(
            matches!(error, StreamPreflightError::Semantic(message) if message.contains("429 Too Many Requests"))
        );
    }

    #[test]
    fn stream_preflight_network_errors_keep_network_classification() {
        let error = StreamPreflightError::Network("连接超时".to_string());

        assert!(error.to_string().contains("网络失败"));
        assert_eq!(
            classify_failure(None, &error.classification_text()).kind,
            HealthFailureKind::NetworkFailed
        );
    }

    #[test]
    fn official_codex_normalizes_standard_string_input_to_item_list() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            Bytes::from_static(
                br#"{"model":"gpt-test","input":"hello","max_output_tokens":16,"temperature":0.2,"top_p":0.9,"truncation":"auto","user":"client-user","context_management":{}}"#,
            ),
            None,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
        assert_eq!(value["parallel_tool_calls"], true);
        for key in [
            "max_output_tokens",
            "temperature",
            "top_p",
            "truncation",
            "user",
            "context_management",
        ] {
            assert!(value.get(key).is_none(), "{key} should be removed");
        }
    }

    #[test]
    fn official_codex_preserves_existing_input_item_list() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let original =
            Bytes::from_static(br#"{"model":"gpt-test","input":[{"role":"user","content":[]}]}"#);
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            original.clone(),
            None,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["store"], false);
        assert_eq!(value["stream"], true);
    }

    #[test]
    fn official_codex_normalizes_cross_provider_input_item_id_prefixes() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            Bytes::from_static(
                br#"{"model":"gpt-test","input":[
                    {"type":"custom_tool_call","id":"item_custom","call_id":"call_1","name":"exec","input":""},
                    {"type":"reasoning","id":"item_reasoning","summary":[]},
                    {"type":"function_call","id":"item_function","call_id":"call_2","name":"lookup","arguments":"{}"},
                    {"type":"message","id":"item_message","role":"assistant","content":[]},
                    {"type":"custom_tool_call","id":"ctc_already_valid","call_id":"call_3","name":"exec","input":""},
                    {"type":"custom_tool_call_output","id":"ctco_output","call_id":"call_1","output":"ok"}
                ]}"#,
            ),
            None,
        );

        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["input"][0]["id"], "ctc_custom");
        assert_eq!(value["input"][1]["id"], "rs_reasoning");
        assert_eq!(value["input"][2]["id"], "fc_function");
        assert_eq!(value["input"][3]["id"], "msg_message");
        assert_eq!(value["input"][4]["id"], "ctc_already_valid");
        assert_eq!(value["input"][5]["id"], "ctco_output");
    }

    #[test]
    fn official_codex_compact_request_preserves_standalone_compaction_semantics() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses/compact".parse().expect("uri");
        let original = Bytes::from_static(
            br#"{"model":"gpt-test","input":[{"role":"user","content":"hello"}]}"#,
        );

        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            original.clone(),
            None,
        );

        assert_eq!(body, original);
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert!(value.get("stream").is_none());
        assert!(value.get("store").is_none());
    }

    #[test]
    fn official_codex_compact_normalizes_cross_provider_input_item_ids() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses/compact".parse().expect("uri");
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            Bytes::from_static(
                br#"{"model":"gpt-test","input":[{"type":"custom_tool_call","id":"item_99fb83474df510b04e475dc5","call_id":"call_1","name":"exec","input":""}]}"#,
            ),
            None,
        );

        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["input"][0]["id"], "ctc_99fb83474df510b04e475dc5");
        assert!(value.get("stream").is_none());
        assert!(value.get("store").is_none());
    }

    #[test]
    fn compact_request_is_not_translated_to_chat_completions() {
        let mut provider = official_provider("https://api.example.com/v1/chat/completions");
        provider.kind = ProviderKind::OpenAiCompatible;
        let uri: Uri = "/v1/responses/compact".parse().expect("uri");

        assert_eq!(
            response_transform(&provider, &Method::POST, &uri),
            ResponseTransform::None
        );
    }

    #[test]
    fn official_codex_uses_real_client_session_for_cache_identity() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", HeaderValue::from_static("session-42"));
        let original = Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#);
        let identity =
            official_codex_session_identity(&provider, &Method::POST, &uri, &headers, &original);
        assert_eq!(identity.as_deref(), Some("session-42"));
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            original,
            identity.as_deref(),
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["prompt_cache_key"], "session-42");
        assert_eq!(value["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn official_codex_preserves_explicit_cache_key() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            Bytes::from_static(
                br#"{"model":"gpt-test","input":"hello","prompt_cache_key":"explicit"}"#,
            ),
            Some("session-42"),
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["prompt_cache_key"], "explicit");
    }

    #[test]
    fn official_codex_converts_system_input_role_to_developer() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        let body = normalize_official_responses_input(
            &provider,
            &Method::POST,
            &uri,
            Bytes::from_static(
                br#"{"model":"gpt-test","input":[{"role":"system","content":[]},{"role":"user","content":[]}]}"#,
            ),
            None,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["input"][0]["role"], "developer");
        assert_eq!(value["input"][1]["role"], "user");
    }

    #[test]
    fn official_codex_bridges_standard_non_stream_responses_requests() {
        let provider = official_provider("https://chatgpt.com/backend-api/codex");
        let uri: Uri = "/v1/responses".parse().expect("uri");
        assert!(official_codex_needs_non_stream_bridge(
            &provider,
            &Method::POST,
            &uri,
            br#"{"model":"gpt-test","input":"hello"}"#,
        ));
        assert!(!official_codex_needs_non_stream_bridge(
            &provider,
            &Method::POST,
            &uri,
            br#"{"model":"gpt-test","input":"hello","stream":true}"#,
        ));
    }

    #[test]
    fn extracts_terminal_response_from_official_codex_sse() {
        let body = b"event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"OK\"}]}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[]}}\n\ndata: [DONE]\n\n";
        let response = terminal_response_from_sse(body).expect("terminal response");
        assert_eq!(response["id"], "resp_1");
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["content"][0]["text"], "OK");
    }

    #[test]
    fn default_to_default_model_map_does_not_rewrite_requested_model() {
        let mut model_map = BTreeMap::new();
        model_map.insert("default".to_string(), "default".to_string());
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map,
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let body = rewrite_model(
            &provider,
            Bytes::from_static(br#"{"model":"gpt-5.5","input":"hello"}"#),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["model"], "gpt-5.5");
    }

    #[test]
    fn default_model_map_does_not_override_user_selected_model() {
        let mut model_map = BTreeMap::new();
        model_map.insert("default".to_string(), "gpt-5.5".to_string());
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map,
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let body = rewrite_model(
            &provider,
            Bytes::from_static(br#"{"model":"gpt-5.4","input":"hello"}"#),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["model"], "gpt-5.4");
    }

    #[test]
    fn relay_provider_v1_base_keeps_responses_requests() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::RelayProvider,
            base_url: "https://api.example.com/v1".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/responses?foo=bar".parse().expect("uri");

        assert_eq!(
            upstream_url(&provider, &uri),
            "https://api.example.com/v1/responses?foo=bar"
        );
        assert_eq!(
            response_transform(&provider, &Method::POST, &uri),
            ResponseTransform::None
        );
    }

    #[test]
    fn explicit_chat_endpoint_uses_chat_completions_transform() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::RelayProvider,
            base_url: "https://api.example.com/v1/chat/completions".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/responses?foo=bar".parse().expect("uri");
        let upstream = upstream_url(&provider, &uri);

        assert_eq!(upstream, "https://api.example.com/v1/chat/completions");
        assert_eq!(
            response_transform(&provider, &Method::POST, &uri),
            ResponseTransform::ChatCompletionsToResponses
        );
        assert_eq!(
            chat_completions_url(&provider, &upstream),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn explicit_responses_endpoint_preserves_query_and_skips_transform() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::RelayProvider,
            base_url: "https://api.example.com/v1/responses?api-version=2026-06-09".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/responses?foo=bar".parse().expect("uri");
        let upstream = upstream_url(&provider, &uri);

        assert_eq!(
            upstream,
            "https://api.example.com/v1/responses?api-version=2026-06-09"
        );
        assert_eq!(
            response_transform(&provider, &Method::POST, &uri),
            ResponseTransform::None
        );
    }

    #[test]
    fn explicit_responses_endpoint_routes_compact_to_the_compact_endpoint() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::RelayProvider,
            base_url: "https://api.example.com/v1/responses?api-version=2026-06-09".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/responses/compact".parse().expect("uri");

        assert_eq!(
            upstream_url(&provider, &uri),
            "https://api.example.com/v1/responses/compact?api-version=2026-06-09"
        );
        assert_eq!(
            response_transform(&provider, &Method::POST, &uri),
            ResponseTransform::None
        );
    }

    #[test]
    fn omitted_stream_defaults_to_non_streaming_in_chat_bridge() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(br#"{"model":"gpt-test","input":"hello"}"#),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["stream"], false);
        assert!(value.get("stream_options").is_none());
    }

    #[test]
    fn converts_responses_body_to_chat_messages() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{"model":"gpt-5-codex","instructions":"be brief","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],"stream":true}"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(value["model"], "gpt-5-codex");
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(value["messages"][0]["content"], "be brief");
        assert_eq!(value["messages"][1]["role"], "user");
        assert_eq!(value["messages"][1]["content"], "hello");
        assert!(value.get("messages").and_then(Value::as_array).is_some());
    }

    #[test]
    fn maps_ultra_to_official_max_effort_for_chat_completions() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{"model":"gpt-5.6-sol","input":"hello","reasoning":{"effort":"ultra"}}"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(value["reasoning_effort"], "max");
        assert!(value.get("reasoning").is_none());
    }

    #[test]
    fn normalizes_chat_function_parameter_root_schema() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{"tools":[{"type":"function","function":{"name":"lookup","parameters":{"properties":{"q":{"type":"string"}}}}}]}"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            value.pointer("/tools/0/function/parameters/type"),
            Some(&Value::String("object".to_string()))
        );
    }

    #[test]
    fn injects_prompt_cache_key_only_for_supported_chat_upstreams() {
        let request =
            Bytes::from_static(br#"{"metadata":{"session_id":"session-123"},"input":"hello"}"#);
        let (enabled, _, _) = responses_body_to_chat_completions(request.clone(), "p", true);
        let (disabled, _, _) = responses_body_to_chat_completions(request, "p", false);
        let enabled: Value = serde_json::from_slice(&enabled).expect("enabled json");
        let disabled: Value = serde_json::from_slice(&disabled).expect("disabled json");
        assert_eq!(enabled["prompt_cache_key"], "session-123");
        assert!(disabled.get("prompt_cache_key").is_none());
    }

    #[test]
    fn converts_codex_tool_types_to_chat_functions_and_messages() {
        let (body, context, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                "model":"gpt-5.4",
                "tools":[
                    {"type":"custom","name":"apply_patch","description":"Apply a patch"},
                    {"type":"tool_search"},
                    {"type":"namespace","name":"github","tools":[
                        {"type":"function","name":"search_issues","parameters":{"type":"object"}}
                    ]}
                ],
                "tool_choice":{"type":"function","name":"search_issues","namespace":"github"},
                "input":[
                    {"type":"function_call","call_id":"call_ns","name":"search_issues","namespace":"github","arguments":"{\"q\":\"bug\"}"},
                    {"type":"function_call_output","call_id":"call_ns","output":{"items":[]}},
                    {"type":"custom_tool_call","call_id":"call_custom","name":"apply_patch","input":"*** Begin Patch"},
                    {"type":"custom_tool_call_output","call_id":"call_custom","output":"ok"},
                    {"type":"tool_search_call","call_id":"call_search","arguments":{"query":"gmail"}},
                    {"type":"tool_search_output","call_id":"call_search","output":[]}
                ]
            }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let tool_names = value["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(
            tool_names,
            vec!["apply_patch", "tool_search", "github__search_issues"]
        );
        assert_eq!(
            value.pointer("/tool_choice/function/name"),
            Some(&Value::String("github__search_issues".to_string()))
        );
        assert_eq!(
            value.pointer("/messages/0/tool_calls/0/function/name"),
            Some(&Value::String("github__search_issues".to_string()))
        );
        assert_eq!(value["messages"][1]["role"], "tool");
        assert_eq!(
            value.pointer("/messages/2/tool_calls/0/function/name"),
            Some(&Value::String("apply_patch".to_string()))
        );
        assert_eq!(
            context
                .lookup("github__search_issues")
                .map(|spec| &spec.kind),
            Some(&ChatToolKind::Namespace)
        );
    }

    #[test]
    fn attaches_responses_reasoning_to_the_following_assistant_message() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"thinking-model",
                    "input":[
                        {"type":"reasoning","summary":[{"type":"summary_text","text":"first thought"}]},
                        {"type":"message","role":"assistant","content":"First answer."},
                        {"type":"reasoning","summary":[{"type":"summary_text","text":"second thought"}]},
                        {"type":"message","role":"assistant","content":"Second answer."},
                        {"type":"message","role":"user","content":"Continue"}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["reasoning_content"], "first thought");
        assert_eq!(messages[1]["reasoning_content"], "second thought");
        assert!(messages[2].get("reasoning_content").is_none());
    }

    #[test]
    fn preserves_object_and_detail_array_reasoning_fields() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "input":[
                        {"type":"message","role":"assistant","content":"First","reasoning":{"summary":"object thought"}},
                        {"type":"message","role":"assistant","content":"Second","reasoning_details":[{"type":"reasoning.text","text":"detail thought"}]}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["reasoning_content"], "object thought");
        assert_eq!(messages[1]["reasoning_content"], "detail thought");
    }

    #[test]
    fn keeps_reasoning_with_tool_calls_and_the_final_answer() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"thinking-model",
                    "input":[
                        {"type":"reasoning","summary":[{"type":"summary_text","text":"need a tool"}]},
                        {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{}"},
                        {"type":"function_call_output","call_id":"call_1","output":"result"},
                        {"type":"reasoning","summary":[{"type":"summary_text","text":"now answer"}]},
                        {"type":"message","role":"assistant","content":"Done."}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["reasoning_content"], "need a tool");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[2]["reasoning_content"], "now answer");
    }

    #[test]
    fn appends_trailing_reasoning_before_a_user_turn_boundary() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "input":[
                        {"type":"message","role":"assistant","content":"Done.","reasoning_content":"embedded"},
                        {"type":"reasoning","summary":[{"type":"summary_text","text":"trailing"}]},
                        {"type":"message","role":"user","content":"Continue"}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["reasoning_content"], "embedded\n\ntrailing");
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn forwards_tool_result_images_as_native_chat_media() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"gpt-5.6-sol",
                    "input":[
                        {"type":"function_call","call_id":"call_image","name":"view_image","arguments":"{}"},
                        {"type":"function_call_output","call_id":"call_image","output":[
                            {"type":"image","mimeType":"image/png","data":"aGVsbG8="},
                            {"type":"text","text":"preview ready"}
                        ]}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[1]["role"], "tool");
        assert!(!messages[1]["content"]
            .as_str()
            .expect("tool content")
            .contains("aGVsbG8="));
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(
            messages[2].pointer("/content/1/image_url/url"),
            Some(&Value::String("data:image/png;base64,aGVsbG8=".to_string()))
        );
    }

    #[test]
    fn forwards_json_encoded_and_remote_tool_result_images() {
        let output = json!({
            "content": [{
                "type": "input_image",
                "image_url": {
                    "url": "https://example.com/preview.png",
                    "detail": "high"
                }
            }]
        })
        .to_string();
        let (content, media) = tool_output_content_and_media(&Value::String(output));

        assert!(!content.contains("https://example.com/preview.png"));
        assert_eq!(media.len(), 1);
        assert_eq!(
            media[0].pointer("/image_url/url"),
            Some(&Value::String(
                "https://example.com/preview.png".to_string()
            ))
        );
        assert_eq!(
            media[0].pointer("/image_url/detail"),
            Some(&Value::String("high".to_string()))
        );
    }

    #[test]
    fn forwards_large_whole_string_tool_image_data_url() {
        let data_url = format!("data:image/png;base64,{}", "aGVsbG8=".repeat(1_100));
        let (content, media) = tool_output_content_and_media(&Value::String(data_url.clone()));

        assert_eq!(content, TOOL_RESULT_IMAGE_MARKER);
        assert_eq!(
            media[0].pointer("/image_url/url").and_then(Value::as_str),
            Some(data_url.as_str())
        );
    }

    #[test]
    fn media_tool_outputs_clamp_residual_base64_but_text_only_outputs_are_stable() {
        let text_only = r#"{ "content": [{"type":"text","text":"ordinary result"}] }"#;
        let (content, media) = tool_output_content_and_media(&Value::String(text_only.to_string()));
        assert_eq!(content, text_only);
        assert!(media.is_empty());

        let bare_base64 = "A".repeat(BASE64ISH_MIN_BYTES);
        let (content, media) = tool_output_content_and_media(&Value::String(bare_base64.clone()));
        assert_eq!(content, bare_base64);
        assert!(media.is_empty());

        let (content, media) = tool_output_content_and_media(&json!({
            "content": [
                {"type":"image","mimeType":"image/png","data":"aGVsbG8="},
                {"type":"text","text":"caption"}
            ],
            "raw": "A".repeat(BASE64ISH_MIN_BYTES)
        }));
        assert_eq!(media.len(), 1);
        assert!(content.contains("omitted"));
        assert!(!content.contains(&"A".repeat(BASE64ISH_MIN_BYTES)));
    }

    #[test]
    fn flushes_parallel_tool_media_after_the_complete_tool_batch() {
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "input":[
                        {"type":"function_call","call_id":"call_1","name":"first","arguments":"{}"},
                        {"type":"function_call","call_id":"call_2","name":"second","arguments":"{}"},
                        {"type":"function_call_output","call_id":"call_1","output":{"type":"image","mimeType":"image/png","data":"aGVsbG8="}},
                        {"type":"function_call_output","call_id":"call_2","output":"done"},
                        {"type":"message","role":"user","content":"continue"}
                    ]
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let messages = value["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"].as_array().map(Vec::len), Some(2));
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"][1]["type"], "image_url");
        assert_eq!(messages[4]["role"], "user");
        assert_eq!(messages[4]["content"], "continue");
    }

    #[test]
    fn forwards_anthropic_shaped_tool_images() {
        let (content, media) = tool_output_content_and_media(&json!({
            "content": [{
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": "YWJj"
                }
            }]
        }));

        assert!(!content.contains("YWJj"));
        assert_eq!(
            media[0].pointer("/image_url/url"),
            Some(&Value::String("data:image/jpeg;base64,YWJj".to_string()))
        );
    }

    #[test]
    fn collects_additional_tools_from_top_level_input_and_overrides() {
        let (body, context, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"gpt-5.4",
                    "additional_tools":[
                        {"type":"custom","name":"top_level_tool"}
                    ],
                    "input":{
                        "type":"message",
                        "role":"user",
                        "content":"use tools",
                        "additional_tools":[
                            {"type":"namespace","name":"github","tools":[
                                {"type":"function","name":"search_issues","parameters":{"type":"object"}}
                            ]}
                        ]
                    },
                    "override":{
                        "additional_tools":[
                            {"type":"function","name":"override_tool","parameters":{"type":"object"}}
                        ]
                    },
                    "overrides":[{
                        "additional_tools":[{"type":"tool_search"}]
                    }],
                    "tool_choice":{"type":"function","name":"search_issues","namespace":"github"}
                }"#,
            ),
            "p",
            false,
        );
        let value: Value = serde_json::from_slice(&body).expect("json");
        let tool_names = value["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(
            tool_names,
            vec![
                "top_level_tool",
                "github__search_issues",
                "override_tool",
                "tool_search"
            ]
        );
        assert_eq!(
            value.pointer("/tool_choice/function/name"),
            Some(&Value::String("github__search_issues".to_string()))
        );
        assert_eq!(
            value.pointer("/messages/0/content"),
            Some(&json!("use tools"))
        );
        assert_eq!(
            context
                .lookup("github__search_issues")
                .map(|spec| &spec.kind),
            Some(&ChatToolKind::Namespace)
        );
    }

    #[test]
    fn restores_custom_tool_search_and_namespace_calls_from_chat_response() {
        let request = json!({
            "tools": [
                {"type":"custom","name":"apply_patch"},
                {"type":"tool_search"},
                {"type":"namespace","name":"github","tools":[
                    {"type":"function","name":"search_issues","parameters":{"type":"object"}}
                ]}
            ]
        });
        let context = ChatToolContext::from_request(&request);
        let response = chat_json_to_responses_json(
            json!({
                "id": "chatcmpl_tools",
                "model": "gpt-5.4",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_custom",
                                "type": "function",
                                "function": {"name":"apply_patch","arguments":"{\"input\":\"patch text\"}"}
                            },
                            {
                                "id": "call_search",
                                "type": "function",
                                "function": {"name":"tool_search","arguments":"{\"query\":\"gmail\",\"limit\":5}"}
                            },
                            {
                                "id": "call_ns",
                                "type": "function",
                                "function": {"name":"github__search_issues","arguments":"{\"q\":\"bug\"}"}
                            }
                        ]
                    }
                }]
            }),
            &context,
        );

        assert_eq!(response["output"][0]["type"], "custom_tool_call");
        assert_eq!(response["output"][0]["input"], "patch text");
        assert_eq!(response["output"][1]["type"], "tool_search_call");
        assert_eq!(response["output"][1]["arguments"]["query"], "gmail");
        assert_eq!(response["output"][2]["type"], "function_call");
        assert_eq!(response["output"][2]["name"], "search_issues");
        assert_eq!(response["output"][2]["namespace"], "github");
    }

    #[test]
    fn restores_previous_response_history_for_tool_result_follow_up() {
        let provider_id = "history-provider";
        let (first_body, first_context, first_messages) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"gpt-5.4",
                    "tools":[{"type":"custom","name":"apply_patch"}],
                    "input":"fix it"
                }"#,
            ),
            provider_id,
            false,
        );
        let first_request: Value = serde_json::from_slice(&first_body).expect("json");
        assert_eq!(first_request["messages"][0]["content"], "fix it");

        let chat_response = json!({
            "id": "chatcmpl_history",
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_patch",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"input\":\"patch\"}"
                        }
                    }]
                }
            }]
        });
        let converted = chat_json_to_responses_json(chat_response.clone(), &first_context);
        store_non_stream_chat_history(
            provider_id,
            &converted,
            &chat_response,
            &first_context,
            &first_messages,
            None,
        );

        let (follow_up_body, follow_up_context, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{
                    "model":"gpt-5.4",
                    "previous_response_id":"resp_chatcmpl_history",
                    "input":[{
                        "type":"custom_tool_call_output",
                        "call_id":"call_patch",
                        "output":"ok"
                    }]
                }"#,
            ),
            provider_id,
            false,
        );
        let follow_up: Value = serde_json::from_slice(&follow_up_body).expect("json");

        assert_eq!(follow_up["messages"][0]["content"], "fix it");
        assert_eq!(
            follow_up.pointer("/messages/1/tool_calls/0/function/name"),
            Some(&Value::String("apply_patch".to_string()))
        );
        assert_eq!(follow_up["messages"][2]["role"], "tool");
        assert_eq!(
            follow_up_context
                .lookup("apply_patch")
                .map(|spec| &spec.kind),
            Some(&ChatToolKind::Custom)
        );
    }

    #[test]
    fn restores_previous_response_history_from_sqlite_after_memory_cache_is_cleared() {
        let temp = tempfile::tempdir().expect("temp");
        let config_store = ConfigStore::new(temp.path().join("config.json"));
        let api_service = ApiServiceStore::from_config_store(&config_store);
        api_service.initialize().expect("initialize api service");
        let provider_id = "persistent-history-provider";
        let (first_body, first_context, first_messages) =
            responses_body_to_chat_completions_with_store(
                Bytes::from_static(
                    br#"{
                        "model":"gpt-5.4",
                        "tools":[{"type":"custom","name":"apply_patch"}],
                        "input":"persist this context"
                    }"#,
                ),
                provider_id,
                false,
                Some(&api_service),
            );
        let first_request: Value = serde_json::from_slice(&first_body).expect("first request");
        assert_eq!(
            first_request["messages"][0]["content"],
            "persist this context"
        );

        let chat_response = json!({
            "id": "chatcmpl_persisted",
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_persisted",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"input\":\"patch\"}"
                        }
                    }]
                }
            }]
        });
        let converted = chat_json_to_responses_json(chat_response.clone(), &first_context);
        store_non_stream_chat_history(
            provider_id,
            &converted,
            &chat_response,
            &first_context,
            &first_messages,
            Some(&api_service),
        );

        let cache = CHAT_HISTORY.get_or_init(|| Mutex::new(ChatHistoryStore::default()));
        cache
            .lock()
            .expect("chat history cache")
            .entries
            .remove(&chat_history_key(provider_id, "resp_chatcmpl_persisted"));

        let (follow_up_body, follow_up_context, _) = responses_body_to_chat_completions_with_store(
            Bytes::from_static(
                br#"{
                        "model":"gpt-5.4",
                        "previous_response_id":"resp_chatcmpl_persisted",
                        "input":[{
                            "type":"custom_tool_call_output",
                            "call_id":"call_persisted",
                            "output":"ok"
                        }]
                    }"#,
            ),
            provider_id,
            false,
            Some(&api_service),
        );
        let follow_up: Value = serde_json::from_slice(&follow_up_body).expect("follow-up");

        assert_eq!(follow_up["messages"][0]["content"], "persist this context");
        assert_eq!(
            follow_up.pointer("/messages/1/tool_calls/0/function/name"),
            Some(&Value::String("apply_patch".to_string()))
        );
        assert_eq!(follow_up["messages"][2]["role"], "tool");
        assert_eq!(
            follow_up_context
                .lookup("apply_patch")
                .map(|spec| &spec.kind),
            Some(&ChatToolKind::Custom)
        );
    }

    #[test]
    fn chat_sse_transform_emits_response_completed() {
        let mut state = ChatSseTransformState::default();
        state.push_chunk(
            br#"data: {"id":"chatcmpl_1","model":"deepseek-chat","choices":[{"delta":{"content":"ok"},"finish_reason":null}]}

"#,
        );
        state.push_chunk(
            br#"data: {"id":"chatcmpl_tools","model":"gpt-5.4","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}

"#,
        );
        state.push_chunk(
            br#"data: [DONE]

"#,
        );
        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();

        assert!(output.contains("response.output_text.delta"));
        assert!(output.contains("response.completed"));
        assert!(output.contains("ok"));
    }

    #[test]
    fn chat_sse_transform_preserves_utf8_across_transport_chunks() {
        let mut state = ChatSseTransformState::default();
        let event = concat!(
            "data: {\"id\":\"chatcmpl_utf8\",\"model\":\"deepseek-chat\",",
            "\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\n"
        )
        .as_bytes();
        let split = event
            .windows("你".len())
            .position(|window| window == "你".as_bytes())
            .expect("utf8 character")
            + 1;

        state.push_chunk(&event[..split]);
        state.push_chunk(&event[split..]);
        state.finish_stream();

        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        assert!(output.contains("你好"));
        assert!(!output.contains('\u{fffd}'));
        assert!(output.contains("response.completed"));
    }

    #[test]
    fn chat_sse_transform_reports_incomplete_eof_as_failed() {
        let mut state = ChatSseTransformState::default();
        state.push_chunk(
            br#"data: {"id":"chatcmpl_cut","model":"deepseek-chat","choices":[{"delta":{"content":"partial"},"finish_reason":null}]}

"#,
        );

        state.finish_stream();

        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_incomplete"));
        assert!(!output.contains("response.completed"));
    }

    #[test]
    fn direct_responses_sse_observer_reports_incomplete_eof() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.keep().join("config.json"));
        store.load().expect("initialize config");
        let mut state = ResponsesSseObserverState::new(
            store.clone(),
            "official".to_string(),
            "request-1".to_string(),
        );
        state.push_chunk(
            br#"data: {"type":"response.created","response":{"id":"resp_1","model":"gpt-test"}}

"#,
        );

        state.finish_stream();

        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        assert!(output.contains("response.failed"));
        assert!(output.contains("upstream_stream_incomplete"));
        assert_eq!(
            store
                .load()
                .expect("config")
                .health
                .get("official")
                .and_then(|health| health.last_failure_kind.clone()),
            Some(codex_companion_core::HealthFailureKind::UpstreamFailed)
        );
    }

    #[test]
    fn direct_responses_sse_observer_accepts_terminal_event_across_chunks() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.keep().join("config.json"));
        store.load().expect("initialize config");
        let mut state = ResponsesSseObserverState::new(
            store.clone(),
            "official".to_string(),
            "request-2".to_string(),
        );
        let event =
            br#"data: {"type":"response.completed","response":{"id":"resp_2","status":"completed"}}

"#;
        let split = event.len() / 2;
        state.push_chunk(&event[..split]);
        state.push_chunk(&event[split..]);
        state.finish_stream();

        assert!(state.pending.is_empty());
        assert!(state.terminal);
        assert!(!store
            .load()
            .expect("config")
            .health
            .contains_key("official"));
    }

    #[test]
    fn chat_sse_transform_restores_custom_and_tool_search_calls() {
        let context = ChatToolContext::from_request(&json!({
            "tools": [
                {"type":"custom","name":"apply_patch"},
                {"type":"tool_search"}
            ]
        }));
        let mut state = ChatSseTransformState::new("p".to_string(), context, Vec::new());
        state.push_chunk(
            br#"data: {"id":"chatcmpl_tools","model":"gpt-5.4","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_custom","type":"function","function":{"name":"apply_patch","arguments":"{\"input\":\"patch\"}"}},{"index":1,"id":"call_search","type":"function","function":{"name":"tool_search","arguments":"{\"query\":\"gmail\"}"}}]},"finish_reason":"tool_calls"}]}

"#,
        );
        state.push_chunk(
            br#"data: {"id":"chatcmpl_tools","model":"gpt-5.4","choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}

"#,
        );
        state.push_chunk(
            br#"data: [DONE]

"#,
        );
        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();

        assert!(output.contains("response.custom_tool_call_input.done"));
        assert!(output.contains("\"type\":\"custom_tool_call\""));
        assert!(output.contains("\"type\":\"tool_search_call\""));
        assert!(output.contains("\"query\":\"gmail\""));
        assert!(output.contains("\"input_tokens\":12"));
        assert!(output.contains("response.completed"));
    }

    #[test]
    fn chat_sse_transform_preserves_late_tool_identity_and_sparse_order() {
        let mut state = ChatSseTransformState::default();
        state.push_chunk(
            br#"data: {"id":"chatcmpl_sparse","choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"tool_b","arguments":"{\"b\":"}},{"index":2,"id":"call_c","function":{"name":"tool_c","arguments":"{}"}}]}}]}

"#,
        );
        state.push_chunk(
            br#"data: {"id":"chatcmpl_sparse","choices":[{"delta":{"tool_calls":[{"index":1,"id":"","function":{"name":"","arguments":"1}"}},{"index":0,"function":{"name":"","arguments":"{}"}}]}}]}

"#,
        );
        state.push_chunk(
            br#"data: {"id":"chatcmpl_sparse","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"tool_a","arguments":""}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
        );
        let output = state
            .pending
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        let tool_a = output.find("\"name\":\"tool_a\"").expect("tool a");
        let tool_b = output.find("\"name\":\"tool_b\"").expect("tool b");
        let tool_c = output.find("\"name\":\"tool_c\"").expect("tool c");
        assert!(tool_a < tool_b && tool_b < tool_c);
        assert!(output.contains("{\\\"b\\\":1}"));
        assert!(!output.contains("\"output_index\":4"));
    }

    #[test]
    fn official_codex_url_gets_client_version() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/models?foo=bar".parse().expect("uri");
        assert_eq!(
            upstream_url(&provider, &uri),
            format!(
                "https://chatgpt.com/backend-api/codex/models?foo=bar&client_version={}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn missing_chat_response_id_uses_stable_semantic_hash() {
        let response = json!({
            "model": "gpt-test",
            "choices": [{"message": {"role": "assistant", "content": "same"}}]
        });
        let first = chat_json_to_responses_json(response.clone(), &ChatToolContext::default());
        let second = chat_json_to_responses_json(response, &ChatToolContext::default());
        let first_id = first.get("id").and_then(Value::as_str).expect("first id");
        assert_eq!(second.get("id").and_then(Value::as_str), Some(first_id));
        assert!(first_id.starts_with("resp_cc_"));
    }
}
