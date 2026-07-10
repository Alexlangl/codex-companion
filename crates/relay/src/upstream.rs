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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};

pub(crate) struct UpstreamResponse {
    response: reqwest::Response,
    transform: ResponseTransform,
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
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
    let (body, tool_context, chat_messages) =
        if transform == ResponseTransform::ChatCompletionsToResponses {
            responses_body_to_chat_completions(body, &provider.id)
        } else {
            (body, ChatToolContext::default(), Vec::new())
        };
    request
        .body(body)
        .send()
        .await
        .map_err(|error| error.to_string())
        .map(|response| UpstreamResponse {
            response,
            transform,
            tool_context,
            chat_messages,
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
            return chat_sse_response(
                provider_id,
                status,
                upstream.response,
                upstream.tool_context,
                upstream.chat_messages,
            );
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
        let converted = chat_json_to_responses_json(value.clone(), &upstream.tool_context);
        store_non_stream_chat_history(
            &provider_id,
            &converted,
            &value,
            &upstream.tool_context,
            &upstream.chat_messages,
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

    let stream = upstream.response.bytes_stream().map_err(io::Error::other);
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
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
) -> Response {
    let stream = chat_sse_to_responses_stream(
        upstream.bytes_stream(),
        provider_id.clone(),
        tool_context,
        chat_messages,
    );
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
        if let Some(input) = value.get("input") {
            collect_tool_search_output_tools(input, &mut context);
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

#[derive(Debug, Clone)]
struct ChatHistoryEntry {
    messages: Vec<Value>,
    tool_context: ChatToolContext,
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

fn load_chat_history(provider_id: &str, response_id: &str) -> Option<ChatHistoryEntry> {
    let store = CHAT_HISTORY.get_or_init(|| Mutex::new(ChatHistoryStore::default()));
    store
        .lock()
        .ok()?
        .entries
        .get(&chat_history_key(provider_id, response_id))
        .cloned()
}

fn store_chat_history(provider_id: &str, response_id: &str, entry: ChatHistoryEntry) {
    if response_id.trim().is_empty() {
        return;
    }
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

fn responses_body_to_chat_completions(
    body: Bytes,
    provider_id: &str,
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
        .and_then(|response_id| load_chat_history(provider_id, response_id));
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
    match input {
        Some(Value::String(text)) => {
            messages.push(json!({ "role": "user", "content": text }));
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_response_input_item(item, messages, &mut pending_tool_calls, tool_context);
            }
        }
        Some(Value::Object(_)) => append_response_input_item(
            input.expect("object input"),
            messages,
            &mut pending_tool_calls,
            tool_context,
        ),
        _ => {}
    }
    flush_pending_tool_calls(messages, &mut pending_tool_calls);
}

fn append_response_input_item(
    item: &Value,
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    tool_context: &ChatToolContext,
) {
    let Some(object) = item.as_object() else {
        flush_pending_tool_calls(messages, pending_tool_calls);
        if let Some(text) = value_text(item) {
            messages.push(json!({ "role": "user", "content": text }));
        }
        return;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("function_call") => {
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
            pending_tool_calls.push(json!({
                "id": response_call_id(object),
                "type": "function",
                "function": {
                    "name": TOOL_SEARCH_PROXY_NAME,
                    "arguments": json_argument_string(object.get("arguments"))
                }
            }));
        }
        Some("message") | None => {
            flush_pending_tool_calls(messages, pending_tool_calls);
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
        Some("function_call_output")
        | Some("custom_tool_call_output")
        | Some("tool_search_output") => {
            flush_pending_tool_calls(messages, pending_tool_calls);
            let content = object
                .get("output")
                .or_else(|| object.get("content"))
                .map(json_value_string)
                .unwrap_or_else(|| json_value_string(item));
            let mut message = json!({ "role": "tool", "content": content });
            if let Some(call_id) = object.get("call_id").and_then(Value::as_str) {
                message["tool_call_id"] = Value::String(call_id.to_string());
            }
            messages.push(message);
        }
        Some("input_text") | Some("output_text") => {
            flush_pending_tool_calls(messages, pending_tool_calls);
            if let Some(text) = object.get("text").and_then(Value::as_str) {
                messages.push(json!({ "role": "user", "content": text }));
            }
        }
        _ => {
            flush_pending_tool_calls(messages, pending_tool_calls);
        }
    }
}

fn flush_pending_tool_calls(messages: &mut Vec<Value>, pending_tool_calls: &mut Vec<Value>) {
    if pending_tool_calls.is_empty() {
        return;
    }
    messages.push(json!({
        "role": "assistant",
        "content": null,
        "tool_calls": std::mem::take(pending_tool_calls)
    }));
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
    Some(json!({ "type": "function", "function": function }))
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
    upstream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    provider_id: String,
    tool_context: ChatToolContext,
    chat_messages: Vec<Value>,
) -> ResponseByteStream {
    let upstream = Box::pin(upstream);
    let state = ChatSseTransformState::new(provider_id, tool_context, chat_messages);
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
                        return Some((Err(io::Error::other(error)), (state, upstream)));
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
    provider_id: String,
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
            buffer: String::new(),
            pending: VecDeque::new(),
            response_id: "resp_codex_companion".to_string(),
            model: String::new(),
            created_at: unix_now(),
            text: String::new(),
            started: false,
            completed: false,
            latest_usage: Value::Null,
            provider_id,
            tool_context,
            chat_messages,
            tools: BTreeMap::new(),
        }
    }

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
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    self.append_tool_call_delta(tool_call);
                }
            }
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
        );
    }

    fn emit_completed_tools(&mut self) {
        let tools = self
            .tools
            .iter()
            .map(|(index, state)| (*index, state.clone()))
            .collect::<Vec<_>>();
        for (position, (chat_index, state)) in tools.into_iter().enumerate() {
            if state.name.trim().is_empty() {
                continue;
            }
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
        let (body, _, _) = responses_body_to_chat_completions(
            Bytes::from_static(
                br#"{"model":"gpt-5-codex","instructions":"be brief","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],"stream":true}"#,
            ),
            "p",
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
