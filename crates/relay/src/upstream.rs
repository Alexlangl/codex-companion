use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::{
    provider_base_url_is_endpoint, provider_endpoint_is_chat_completions, ProviderConfig,
    ProviderKind,
};
use codex_companion_provider::{ensure_codex_auth_snapshot, resolve_auth_token};
use futures_util::{stream, Stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;

pub(crate) struct UpstreamResponse {
    response: reqwest::Response,
    transform: ResponseTransform,
}

impl UpstreamResponse {
    pub(crate) fn status(&self) -> StatusCode {
        self.response.status()
    }

    pub(crate) async fn text(self) -> Result<String, reqwest::Error> {
        self.response.text().await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTransform {
    None,
    ChatCompletionsToResponses,
}

pub(crate) async fn send_upstream(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
    upstream: &str,
) -> std::result::Result<UpstreamResponse, String> {
    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|error| format!("invalid method: {error}"))?;
    let transform = response_transform(provider, method, uri);
    let upstream = if transform == ResponseTransform::ChatCompletionsToResponses {
        chat_completions_url(provider, upstream)
    } else {
        upstream.to_string()
    };
    let mut request = client.request(reqwest_method, &upstream);
    for (name, value) in headers {
        if matches!(
            *name,
            header::HOST | header::AUTHORIZATION | header::CONTENT_LENGTH
        ) {
            continue;
        }
        request = request.header(name, value);
    }
    if provider.kind == ProviderKind::OfficialCodex {
        let auth = ensure_codex_auth_snapshot(provider)
            .await
            .map_err(|error| error.to_string())?;
        request = request.bearer_auth(auth.access_token);
        if let Some(account_id) = provider
            .account
            .as_ref()
            .and_then(|account| account.account_id.clone())
            .or(auth.account_id)
        {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        request = request.header("originator", "codex-companion");
    } else if let Some(token) = resolve_auth_token(provider) {
        request = request.bearer_auth(token);
    }
    let body = rewrite_model(provider, body);
    let body = if transform == ResponseTransform::ChatCompletionsToResponses {
        responses_body_to_chat_completions(body)
    } else {
        body
    };
    request
        .body(body)
        .send()
        .await
        .map_err(|error| error.to_string())
        .map(|response| UpstreamResponse {
            response,
            transform,
        })
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

pub(crate) async fn stream_response(provider_id: String, upstream: UpstreamResponse) -> Response {
    let status = upstream.response.status();
    let headers = upstream.response.headers().clone();
    if upstream.transform == ResponseTransform::ChatCompletionsToResponses {
        if is_event_stream(&headers) {
            return chat_sse_response(provider_id, status, upstream.response);
        }
        let body = match upstream.response.text().await {
            Ok(body) => body,
            Err(error) => {
                return text_response(
                    StatusCode::BAD_GATEWAY,
                    format!("读取上游响应失败: {error}"),
                )
            }
        };
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
        let body = chat_json_to_responses_json(value).to_string();
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

    let stream = upstream
        .response
        .bytes_stream()
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error));
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

fn chat_sse_response(
    provider_id: String,
    status: StatusCode,
    upstream: reqwest::Response,
) -> Response {
    let stream = chat_sse_to_responses_stream(upstream.bytes_stream());
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

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

fn response_transform(provider: &ProviderConfig, method: &Method, uri: &Uri) -> ResponseTransform {
    if method == Method::POST
        && uri
            .path_and_query()
            .is_some_and(|value| is_responses_url(value.as_str()))
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

fn responses_body_to_chat_completions(body: Bytes) -> Bytes {
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return body;
    };
    let Some(object) = value.as_object() else {
        return body;
    };

    let mut output = serde_json::Map::new();
    copy_json_field(object, &mut output, "model");
    copy_json_field(object, &mut output, "temperature");
    copy_json_field(object, &mut output, "top_p");
    copy_json_field(object, &mut output, "presence_penalty");
    copy_json_field(object, &mut output, "frequency_penalty");
    copy_json_field(object, &mut output, "parallel_tool_calls");
    copy_json_field(object, &mut output, "tool_choice");
    copy_json_field(object, &mut output, "response_format");
    copy_json_field(object, &mut output, "metadata");
    if let Some(max_output_tokens) = object.get("max_output_tokens") {
        output.insert("max_tokens".to_string(), max_output_tokens.clone());
    }
    output.insert(
        "stream".to_string(),
        object.get("stream").cloned().unwrap_or(Value::Bool(true)),
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

    let mut messages = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(value_text) {
        if !instructions.is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }
    append_response_input_messages(object.get("input"), &mut messages);
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    output.insert("messages".to_string(), Value::Array(messages));

    if let Some(tools) = object.get("tools").and_then(responses_tools_to_chat_tools) {
        output.insert("tools".to_string(), tools);
    }

    serde_json::to_vec(&Value::Object(output))
        .map(Bytes::from)
        .unwrap_or(body)
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

fn append_response_input_messages(input: Option<&Value>, messages: &mut Vec<Value>) {
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({ "role": "user", "content": text }));
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_response_input_item(item, messages);
            }
        }
        Some(Value::Object(_)) => append_response_input_item(input.unwrap(), messages),
        _ => {}
    }
}

fn append_response_input_item(item: &Value, messages: &mut Vec<Value>) {
    let Some(object) = item.as_object() else {
        if let Some(text) = value_text(item) {
            messages.push(json!({ "role": "user", "content": text }));
        }
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("message") | None => {
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
            messages.push(json!({ "role": role, "content": content }));
        }
        Some("function_call_output") => {
            let content = object
                .get("output")
                .and_then(value_text)
                .or_else(|| object.get("content").and_then(value_text))
                .unwrap_or_default();
            let mut message = json!({ "role": "tool", "content": content });
            if let Some(call_id) = object.get("call_id").and_then(Value::as_str) {
                message["tool_call_id"] = Value::String(call_id.to_string());
            }
            messages.push(message);
        }
        Some("input_text") | Some("output_text") => {
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                messages.push(json!({ "role": "user", "content": text }));
            }
        }
        _ => {}
    }
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

fn responses_tools_to_chat_tools(value: &Value) -> Option<Value> {
    let tools = value.as_array()?;
    let chat_tools = tools
        .iter()
        .filter_map(|tool| {
            let object = tool.as_object()?;
            if object.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let name = object.get("name").and_then(Value::as_str)?;
            let description = object
                .get("description")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));
            let parameters = object
                .get("parameters")
                .or_else(|| object.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            }))
        })
        .collect::<Vec<_>>();
    (!chat_tools.is_empty()).then(|| Value::Array(chat_tools))
}

fn chat_json_to_responses_json(value: Value) -> Value {
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
        .unwrap_or_else(|| "resp_codex_companion".to_string());
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
    json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": "completed",
        "model": model,
        "output": [{
            "id": "msg_codex_companion",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }],
        "usage": usage
    })
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
    upstream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
) -> ResponseByteStream {
    let upstream = Box::pin(upstream);
    let state = ChatSseTransformState::default();
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
                        return Some((
                            Err(io::Error::new(io::ErrorKind::Other, error)),
                            (state, upstream),
                        ));
                    }
                    None => {
                        state.finish();
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
    buffer: String,
    pending: VecDeque<Bytes>,
    response_id: String,
    model: String,
    created_at: i64,
    text: String,
    started: bool,
    completed: bool,
    latest_usage: Value,
}

impl Default for ChatSseTransformState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            pending: VecDeque::new(),
            response_id: "resp_codex_companion".to_string(),
            model: String::new(),
            created_at: unix_now(),
            text: String::new(),
            started: false,
            completed: false,
            latest_usage: Value::Null,
        }
    }
}

impl ChatSseTransformState {
    fn push_chunk(&mut self, chunk: &[u8]) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        while let Some(index) = self.next_block_index() {
            let block = self.buffer[..index].to_string();
            let drain_to = if self.buffer[index..].starts_with("\r\n\r\n") {
                index + 4
            } else {
                index + 2
            };
            self.buffer.drain(..drain_to);
            self.process_block(&block);
        }
    }

    fn next_block_index(&self) -> Option<usize> {
        match (self.buffer.find("\n\n"), self.buffer.find("\r\n\r\n")) {
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
        }
        if choice
            .and_then(|choice| choice.get("finish_reason"))
            .is_some_and(|reason| !reason.is_null())
        {
            self.finish();
        }
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
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
        self.emit(json!({
            "type": "response.completed",
            "response": self.response_object("completed")
        }));
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
                json!([self.message_item(true)])
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

fn response_id_from_chat_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_string()
    } else {
        format!("resp_{id}")
    }
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
    use codex_companion_core::{default_refresh_interval_seconds, ProviderKind};
    use std::collections::BTreeMap;

    #[test]
    fn builds_upstream_url_from_v1_base() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
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
    fn rewrites_request_model_from_provider_map() {
        let mut model_map = BTreeMap::new();
        model_map.insert("gpt-5-codex".to_string(), "deepseek-chat".to_string());
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
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
            Bytes::from_static(br#"{"model":"gpt-5-codex","input":"hello"}"#),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["model"], "deepseek-chat");
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
    fn converts_responses_body_to_chat_messages() {
        let body = responses_body_to_chat_completions(Bytes::from_static(
            br#"{"model":"gpt-5-codex","instructions":"be brief","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],"stream":true}"#,
        ));
        let value: Value = serde_json::from_slice(&body).expect("json");

        assert_eq!(value["model"], "gpt-5-codex");
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(value["messages"][0]["content"], "be brief");
        assert_eq!(value["messages"][1]["role"], "user");
        assert_eq!(value["messages"][1]["content"], "hello");
        assert!(value.get("messages").and_then(Value::as_array).is_some());
    }

    #[test]
    fn chat_sse_transform_emits_response_completed() {
        let mut state = ChatSseTransformState::default();
        state.push_chunk(
            br#"data: {"id":"chatcmpl_1","model":"deepseek-chat","choices":[{"delta":{"content":"ok"},"finish_reason":null}]}

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
    fn official_codex_url_gets_client_version() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
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
}
