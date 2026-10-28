use axum::http::{HeaderMap, HeaderValue};
use serde_json::{json, Value};

pub(crate) const HEADER: &str = "x-openai-internal-codex-responses-lite";
const METADATA: &str = "ws_request_header_x_openai_internal_codex_responses_lite";

pub(crate) fn enabled(value: &Value) -> bool {
    value
        .get("client_metadata")
        .and_then(|m| m.get(METADATA))
        .is_some_and(|v| {
            v == true
                || v.as_str()
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case("true"))
        })
}

fn is(value: &Value, expected: &str) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case(expected))
}

fn image(tool: &Value) -> bool {
    is(tool, "image_generation")
        || is(tool, "image_gen.imagegen")
        || is(&tool["type"], "image_generation")
        || is(&tool["name"], "image_gen.imagegen")
        || is(&tool["name"], "image_generation")
        || tool
            .pointer("/function/name")
            .is_some_and(|v| is(v, "image_gen.imagegen"))
        || ((is(&tool["name"], "image_gen") || is(&tool["namespace"], "image_gen"))
            && tool["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|t| is(&t["name"], "imagegen"))))
}

fn uses_image(value: &Value) -> bool {
    value["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(image))
        || value.get("tool_choice").is_some_and(|choice| {
            image(choice)
                || choice["tools"]
                    .as_array()
                    .is_some_and(|tools| tools.iter().any(image))
        })
        || value["input"].as_array().is_some_and(|input| {
            input
                .iter()
                .filter(|item| is(&item["type"], "additional_tools"))
                .any(uses_image)
        })
        || value.get("response").is_some_and(uses_image)
}

fn supported(tool: &Value) -> bool {
    matches!(
        tool["type"].as_str(),
        Some("function" | "custom" | "namespace")
    ) || (is(&tool["type"], "tool_search") && is(&tool["execution"], "client"))
}

fn filter_tools(value: &mut Value, key: &str) {
    if let Some(tools) = value.get_mut(key).and_then(Value::as_array_mut) {
        tools.retain(supported);
        for tool in tools.iter_mut().filter(|t| t["type"] == "namespace") {
            filter_tools(tool, "tools");
        }
        tools.retain(|t| {
            t["type"] != "namespace" || t["tools"].as_array().is_some_and(|tools| !tools.is_empty())
        });
        if tools.is_empty() {
            value.as_object_mut().unwrap().remove(key);
        }
    }
}

fn choice_supported(choice: &mut Value) -> bool {
    if let Some(value) = choice.as_str() {
        return matches!(value, "auto" | "none" | "required");
    }
    if supported(choice) {
        return true;
    }
    if choice["type"] != "allowed_tools" {
        return false;
    }
    filter_tools(choice, "tools");
    filter_tools(choice, "allowed_tools");
    if let Some(nested) = choice.get_mut("allowed_tools").filter(|v| v.is_object()) {
        filter_tools(nested, "tools");
    }
    ["/tools", "/allowed_tools", "/allowed_tools/tools"]
        .into_iter()
        .any(|path| {
            choice
                .pointer(path)
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty())
        })
}

fn filter(value: &mut Value) {
    filter_tools(value, "tools");
    if value
        .get_mut("tool_choice")
        .is_some_and(|choice| !choice_supported(choice))
    {
        value.as_object_mut().unwrap().remove("tool_choice");
    }
    if let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) {
        for item in input
            .iter_mut()
            .filter(|item| is(&item["type"], "additional_tools"))
        {
            filter(item);
        }
        input.retain(|item| {
            !is(&item["type"], "additional_tools")
                || item["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty())
        });
    }
    if let Some(response) = value.get_mut("response").filter(|v| v.is_object()) {
        filter(response);
    }
}

/// Apply only to protocol fields, never to opaque history or tool outputs.
pub(crate) fn normalize(headers: &mut HeaderMap, value: &mut Value, official: bool, compact: bool) {
    let metadata = enabled(value) || value.get("response").is_some_and(enabled);
    let lite = headers.contains_key(HEADER) || metadata;
    if !official {
        headers.remove(HEADER);
    }
    if compact || !lite || !value.is_object() {
        return;
    }
    if let Some(response) = value.get_mut("response").filter(|v| v.is_object()) {
        response["parallel_tool_calls"] = json!(false);
    } else {
        value["parallel_tool_calls"] = json!(false);
    }
    if !official {
        return;
    }
    if uses_image(value) {
        headers.remove(HEADER);
        if let Some(metadata) = value
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
        {
            metadata.remove(METADATA);
        }
        if let Some(metadata) = value
            .pointer_mut("/response/client_metadata")
            .and_then(Value::as_object_mut)
        {
            metadata.remove(METADATA);
        }
    } else {
        filter(value);
        if metadata {
            headers.insert(HEADER, HeaderValue::from_static("true"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_protocol_tools_but_keeps_history_opaque() {
        let history =
            json!({"type":"function_call_output","output":{"tools":[{"type":"web_search"}]}});
        let mut body = json!({"tools":[{"type":"web_search"},{"type":"function","name":"f"},
            {"type":"custom","name":"c"},{"type":"tool_search","execution":"client"}],
            "input":[history, {"type":"additional_tools","tools":[{"type":"file_search"}]}],
            "tool_choice":{"type":"allowed_tools","tools":[{"type":"web_search"},{"type":"function","name":"f"}]},
            "client_metadata":{METADATA:true}});
        let mut headers = HeaderMap::new();
        normalize(&mut headers, &mut body, true, false);
        assert_eq!(headers[HEADER], "true");
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["tools"].as_array().unwrap().len(), 3);
        assert_eq!(body["input"], json!([history]));
        assert_eq!(body["tool_choice"]["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn image_tools_promote_to_full_responses_and_api_keys_keep_their_tools() {
        for tools in [
            json!([{"type":"image_generation"}]),
            json!([{"type":"namespace","name":"image_gen","tools":[{"type":"function","name":"imagegen"}]}]),
        ] {
            let mut body = json!({"tools":tools,"client_metadata":{METADATA:true}});
            let mut headers = HeaderMap::new();
            headers.insert(HEADER, HeaderValue::from_static("true"));
            normalize(&mut headers, &mut body, true, false);
            assert!(!headers.contains_key(HEADER));
            assert!(!enabled(&body));
            assert_eq!(body["tools"], tools);
        }
        let mut body = json!({"tools":[{"type":"web_search"}]});
        let mut headers = HeaderMap::new();
        headers.insert(HEADER, HeaderValue::from_static("true"));
        normalize(&mut headers, &mut body, false, false);
        assert!(!headers.contains_key(HEADER));
        assert_eq!(body["tools"][0]["type"], "web_search");
    }
}
