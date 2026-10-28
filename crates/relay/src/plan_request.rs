//! Public ChatGPT plan requests have a narrower contract than Codex's native endpoint.
//! Validate capabilities before modifying the per-route copy; never discard history.
use serde_json::{json, Value};

pub(crate) fn validate(value: &Value, websocket: bool) -> Result<(), String> {
    if !value.is_object() {
        return Err("请求体必须为 JSON 对象".into());
    }
    if !websocket
        && value
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
    {
        return Err(
            "ChatGPT 套餐 HTTP 请求需要完整 input 历史，不支持 previous_response_id".into(),
        );
    }
    if value
        .get("input")
        .is_some_and(|v| !v.is_array() && !v.is_string())
    {
        return Err("ChatGPT 套餐 input 必须为数组或文本".into());
    }
    tools(value.get("tools"), 0)?;
    choice(value.get("tool_choice"), 0)?;
    for item in value["input"].as_array().into_iter().flatten() {
        if item["type"] == "configuration_update" {
            let fields = item.as_object().ok_or("configuration_update 格式无效")?;
            if fields
                .keys()
                .any(|key| !matches!(key.as_str(), "type" | "id" | "reasoning"))
                || item
                    .get("reasoning")
                    .filter(|v| !v.is_null())
                    .is_some_and(|v| {
                        v.as_object()
                            .is_none_or(|r| r.keys().any(|k| k != "effort"))
                    })
                || item
                    .pointer("/reasoning/effort")
                    .is_some_and(|v| !v.is_null() && !v.is_string())
            {
                return Err(
                    "ChatGPT 套餐的 configuration_update 仅支持调整 reasoning.effort".into(),
                );
            }
        }
        if item["type"] == "additional_tools" {
            tools(item.get("tools"), 0)?;
        }
        if matches!(
            item["type"].as_str(),
            Some("tool_search_call" | "tool_search_output")
        ) {
            return Err(
                "ChatGPT 套餐不支持 Responses tool_search 历史，请使用 function/custom 工具".into(),
            );
        }
        validate_input(item)?;
    }
    Ok(())
}

fn validate_input(item: &Value) -> Result<(), String> {
    if matches!(
        item["type"].as_str(),
        Some("audio" | "input_audio" | "video" | "input_video")
    ) {
        return Err("ChatGPT 套餐不支持音频或视频输入".into());
    }
    if item["type"] == "input_file" && item.get("file_id").is_some() {
        return Err("ChatGPT 套餐文件输入需要内联内容或 URL，不支持已上传的 file_id".into());
    }
    for key in ["content", "output"] {
        for part in item[key].as_array().into_iter().flatten() {
            validate_input(part)?;
        }
    }
    Ok(())
}

fn tools(value: Option<&Value>, depth: usize) -> Result<(), String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(());
    };
    if depth > 16 {
        return Err("工具命名空间层级过深".into());
    }
    for tool in value.as_array().ok_or("tools 必须为数组")? {
        match tool["type"].as_str() {
            Some("function" | "custom") => {
                if tool["allowed_callers"].as_array().is_some_and(|v| v.iter().any(|v| v == "programmatic")) {
                    return Err("ChatGPT 套餐不支持 programmatic 工具调用".into());
                }
            }
            Some("namespace") if tool["name"].as_str().is_some_and(|n| !n.is_empty()) && tool["tools"].is_array() => tools(tool.get("tools"), depth + 1)?,
            Some("web_search" | "web_search_preview" | "web_search_preview_2025_03_11") if depth == 0 => {}
            _ => return Err("ChatGPT 套餐不支持本次请求中的 hosted/tool_search 工具，请改用 function/custom 工具".into()),
        }
    }
    Ok(())
}

fn choice(value: Option<&Value>, depth: usize) -> Result<(), String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(());
    };
    if depth > 16 {
        return Err("tool_choice 层级过深".into());
    }
    if value
        .as_str()
        .is_some_and(|v| matches!(v, "auto" | "none" | "required"))
    {
        return Ok(());
    }
    match value["type"].as_str() {
        Some(
            "function"
            | "custom"
            | "namespace"
            | "web_search"
            | "web_search_preview"
            | "web_search_preview_2025_03_11",
        ) => Ok(()),
        Some("allowed_tools") => {
            for tool in value["tools"]
                .as_array()
                .ok_or("allowed_tools.tools 必须为数组")?
            {
                choice(Some(tool), depth + 1)?;
            }
            Ok(())
        }
        _ => Err("ChatGPT 套餐不支持本次 tool_choice".into()),
    }
}

pub(crate) fn normalize(value: &mut Value, websocket: bool) -> Result<(), String> {
    validate(value, websocket)?;
    let effort = value["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "configuration_update")
        .filter_map(|item| item.pointer("/reasoning/effort").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .last()
        .map(str::to_ascii_lowercase);
    if effort.is_some()
        && value
            .get("reasoning")
            .is_some_and(|v| !v.is_null() && !v.is_object())
    {
        return Err("reasoning 必须为对象".into());
    }
    let object = value.as_object_mut().ok_or("请求体必须为 JSON 对象")?;
    for field in [
        "background",
        "conversation",
        "max_output_tokens",
        "max_completion_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "context_management",
        "prompt_cache_options",
        "client_metadata",
        "generate",
        "stream_options",
    ] {
        object.remove(field);
    }
    object.remove("service_tier");
    object.insert("store".into(), json!(false));
    if !websocket {
        object.insert("stream".into(), json!(true));
    }
    if let Some(input) = object.get_mut("input") {
        if let Some(text) = input.as_str() {
            *input = json!([{"role":"user","content":text}]);
        }
        if !input.is_array() {
            return Err("ChatGPT 套餐 input 必须为数组或文本".into());
        }
        let items = input.as_array_mut().unwrap();
        items.retain(|item| item["type"] != "configuration_update");
        for item in items {
            if item["type"] == "agent_message" {
                item["type"] = json!("message");
                item["role"] = json!("user");
            }
            if let Some(object) = item.as_object_mut() {
                for key in [
                    "author",
                    "recipient",
                    "internal_chat_message_metadata_passthrough",
                ] {
                    object.remove(key);
                }
            }
            if item["type"] == "function_call" && item["arguments"] == "" {
                item["arguments"] = json!("{}");
            }
            if item["type"] == "additional_tools" {
                item["role"] = json!("developer");
            }
            if let Some(parts) = item.get_mut("content").and_then(Value::as_array_mut) {
                for part in parts {
                    if let Some(object) = part.as_object_mut() {
                        object.remove("prompt_cache_breakpoint");
                    }
                }
            }
            if item["role"] == "system" && item.get("type").is_none_or(|t| t == "message") {
                item["role"] = json!("developer");
            }
        }
    } else {
        object.insert("input".into(), json!([]));
    }
    let mut additional = Vec::new();
    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools.iter_mut() {
            if matches!(
                tool["type"].as_str(),
                Some("web_search_preview" | "web_search_preview_2025_03_11")
            ) {
                tool["type"] = json!("web_search");
            }
        }
        let mut retained = Vec::new();
        for tool in std::mem::take(tools) {
            if matches!(tool["type"].as_str(), Some("function" | "custom")) {
                additional.push(tool);
            } else {
                retained.push(tool);
            }
        }
        *tools = retained;
    }
    if !additional.is_empty() {
        object["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"additional_tools","role":"developer","tools":additional}));
    }
    if let Some(effort) = effort {
        let reasoning = object.entry("reasoning").or_insert_with(|| json!({}));
        if reasoning.is_null() {
            *reasoning = json!({});
        }
        reasoning["effort"] = json!(effort);
    }
    Ok(())
}

pub(crate) fn error_status(message: &str) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    for (codes, status) in [
        (
            &[
                "subscription_sharing_usage_limit_reached",
                "subscription_sharing_usage_limit_exceeded",
            ][..],
            StatusCode::TOO_MANY_REQUESTS,
        ),
        (
            &[
                "usage_unavailable",
                "user_unavailable",
                "subscription_sharing_usage_unavailable",
                "subscription_sharing_user_unavailable",
            ][..],
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            &["invalid_user", "subscription_sharing_invalid_user"][..],
            StatusCode::UNAUTHORIZED,
        ),
        (
            &[
                "user_not_eligible",
                "route_not_supported",
                "chatpass_v2_scope_not_authorized",
                "invalid_authorization_context",
                "chatpass_v2_invalid_authorization_context",
                "subscription_sharing_user_not_eligible",
                "subscription_sharing_route_not_supported",
            ][..],
            StatusCode::FORBIDDEN,
        ),
        (
            &[
                "unsupported_capability",
                "subscription_sharing_unsupported_capability",
            ][..],
            StatusCode::BAD_REQUEST,
        ),
    ] {
        if codes.iter().any(|code| {
            message
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == *code)
        }) {
            return status;
        }
    }
    StatusCode::BAD_GATEWAY
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsupported_capabilities_without_removing_history() {
        let original = json!({"tools":[{"type":"image_generation"}],"input":[{"type":"compaction","encrypted_content":"opaque"}]});
        let mut value = original.clone();
        assert!(normalize(&mut value, false).is_err());
        assert_eq!(value, original);
        assert!(validate(&json!({"previous_response_id":"resp_fixture"}), false).is_err());
        assert!(validate(&json!({"previous_response_id":"resp_fixture"}), true).is_ok());
    }
    #[test]
    fn adapts_route_copy_and_keeps_opaque_tool_payloads() {
        let mut value = json!({"stream":false,"service_tier":"priority","temperature":1,"input":[{"role":"system","content":"instructions"},{"type":"function_call_output","output":"{\"type\":\"image_generation\"}"}],"tools":[{"type":"namespace","name":"local","tools":[{"type":"function","name":"read"}]}]});
        normalize(&mut value, false).unwrap();
        assert_eq!(value["stream"], true);
        assert_eq!(value["store"], false);
        assert_eq!(value["input"][0]["role"], "developer");
        assert_eq!(
            value["input"][1]["output"],
            "{\"type\":\"image_generation\"}"
        );
        assert!(value.get("service_tier").is_none());
    }
    #[test]
    fn sharing_errors_keep_the_documented_status() {
        assert_eq!(
            error_status("subscription_sharing_usage_limit_reached retry"),
            axum::http::StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            error_status("subscription_sharing_usage_limit_exceeded"),
            axum::http::StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            error_status("subscription_sharing_user_unavailable"),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            error_status("invalid_user"),
            axum::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            error_status("invalid_user_extra"),
            axum::http::StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            error_status("chatpass_v2_scope_not_authorized"),
            axum::http::StatusCode::FORBIDDEN
        );
    }
    #[test]
    fn public_request_projects_tool_declarations_and_configuration_without_changing_source() {
        let source = json!({"tools":[{"type":"function","name":"read","parameters":{"type":"object"}}],"client_metadata":{"internal":true},"input":[{"type":"configuration_update","reasoning":{"effort":"high"}},{"type":"agent_message","author":"agent","content":[{"type":"input_text","text":"hello","prompt_cache_breakpoint":true}]}]});
        let mut wire = source.clone();
        normalize(&mut wire, false).unwrap();
        assert_eq!(wire["tools"], json!([]));
        assert!(wire.get("client_metadata").is_none());
        assert_eq!(wire["reasoning"]["effort"], "high");
        assert_eq!(wire["input"][0]["type"], "message");
        assert_eq!(wire["input"][1]["type"], "additional_tools");
        assert_eq!(wire["input"][1]["role"], "developer");
        assert_eq!(source["input"][0]["type"], "configuration_update");
        assert!(validate(
            &json!({"input":[{"type":"configuration_update","tools":[]}]}),
            false
        )
        .is_err());
        assert!(validate(&json!({"input":[{"role":"user","content":[{"type":"input_file","file_id":"file_fixture"}]}]}),false).is_err());
    }
}
