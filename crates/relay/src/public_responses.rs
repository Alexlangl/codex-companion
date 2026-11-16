use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize,
};
use serde_json::{json, Value};

/// Public Responses endpoints reject this Codex backend-only include option.
/// Keep encrypted reasoning history and all public include options intact.
pub(crate) fn normalize(value: &mut Value) {
    normalize_items(value);
    if let Some(response) = value.get_mut("response").filter(|v| v.is_object()) {
        normalize(response);
    }
    if let Some(include) = value.get_mut("include").and_then(Value::as_array_mut) {
        let before = include.len();
        include.retain(|v| v.as_str() != Some("output.encrypted_content"));
        if before != include.len() && include.is_empty() {
            value.as_object_mut().unwrap().remove("include");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_private_include_is_removed_from_http_and_ws_payloads() {
        let original = json!({"include":["output.encrypted_content","reasoning.encrypted_content"],
            "input":[{"type":"reasoning","encrypted_content":"opaque"}]});
        for wrapped in [false, true] {
            let mut body = if wrapped {
                json!({"type":"response.create","response":original})
            } else {
                original.clone()
            };
            normalize(&mut body);
            let response = if wrapped { &body["response"] } else { &body };
            assert_eq!(response["include"], json!(["reasoning.encrypted_content"]));
            assert_eq!(response["input"], original["input"]);
            let once = body.clone();
            normalize(&mut body);
            assert_eq!(body, once);
        }
    }
    #[test]
    fn public_history_conversion_preserves_receipts_and_only_deduplicates_arguments() {
        let mut body = json!({"access_programs":[],"stream_options":{"reasoning_summary_delivery":"automatic","other":true},
            "input":[{"type":"configuration_update","reasoning":{"effort":"high"}},
                {"type":"function_call_output","name":"page_context","output":"page text"},
                {"type":"function_call_output","call_id":"real","output":"tool output"},
                {"type":"function_call","arguments":"{\"x\":1,\"x\":2,\"nested\":[{\"z\":true,\"z\":false}]}"},
                {"type":"function_call","arguments":"{ \"x\": 1 }"},
                {"type":"reasoning","encrypted_content":"retain"},
                {"type":"function_call","arguments":"{malformed"}]});
        normalize(&mut body);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("access_programs").is_none());
        assert_eq!(body["stream_options"], json!({"other":true}));
        assert_eq!(body["input"][0]["content"][0]["text"], "page text");
        assert_eq!(body["input"][1]["call_id"], "real");
        assert_eq!(
            serde_json::from_str::<Value>(body["input"][2]["arguments"].as_str().unwrap()).unwrap(),
            json!({"x":2,"nested":[{"z":false}]})
        );
        assert_eq!(body["input"][3]["arguments"], "{ \"x\": 1 }");
        assert_eq!(body["input"][4]["encrypted_content"], "retain");
        assert_eq!(body["input"][5]["arguments"], "{malformed");
    }

    #[test]
    fn recovery_requires_explicit_rejection_and_replayable_plaintext() {
        let request = json!({"input":[{"role":"user","content":"task"},{"type":"reasoning","encrypted_content":"opaque"},
            {"type":"function_call_output","call_id":"c","output":[{"type":"input_text","text":"real result"},{"type":"encrypted_content","encrypted_content":"opaque"}]}]});
        let bytes = request.to_string();
        assert!(recover_history(bytes.as_bytes(), "server overloaded").is_none());
        let fixed = recover_history(bytes.as_bytes(), "invalid_encrypted_content").unwrap();
        let fixed: Value = serde_json::from_slice(&fixed).unwrap();
        assert_eq!(fixed["input"].as_array().unwrap().len(), 2);
        assert_eq!(fixed["input"][1]["output"][0]["text"], "real result");
        assert!(
            recover_history(fixed.to_string().as_bytes(), "invalid_encrypted_content").is_none()
        );
        for item in [
            json!({"type":"compaction","encrypted_content":"only history"}),
            json!({"type":"agent_message","content":[{"type":"encrypted_content","encrypted_content":"only task"}]}),
        ] {
            let mut unsafe_request = request.clone();
            unsafe_request["input"].as_array_mut().unwrap().push(item);
            assert!(recover_history(
                unsafe_request.to_string().as_bytes(),
                "invalid_encrypted_content"
            )
            .is_none());
        }
        let azure = json!({"input":[{"id":"msg_old","role":"user","content":"keep"},{"id":"fc_old","call_id":"c","type":"function_call","name":"read","arguments":"{}"}]});
        let fixed = recover_history(
            azure.to_string().as_bytes(),
            "The requested item was created under a different Azure OpenAI resource.",
        )
        .unwrap();
        let fixed: Value = serde_json::from_slice(&fixed).unwrap();
        assert!(fixed["input"][1].get("id").is_none());
        assert_eq!(fixed["input"][1]["call_id"], "c");
    }
}

fn normalize_items(value: &mut Value) {
    let effort = value["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "configuration_update")
        .filter_map(|item| item.pointer("/reasoning/effort").and_then(Value::as_str))
        .last()
        .map(str::to_owned);
    if let Some(effort) = effort {
        if value.get("reasoning").is_none_or(|v| v.is_null()) {
            value["reasoning"] = json!({});
        }
        if let Some(reasoning) = value["reasoning"].as_object_mut() {
            reasoning.insert("effort".into(), Value::String(effort));
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("access_programs");
    }
    if let Some(options) = value
        .get_mut("stream_options")
        .and_then(Value::as_object_mut)
    {
        options.remove("reasoning_summary_delivery");
        if options.is_empty() {
            value.as_object_mut().unwrap().remove("stream_options");
        }
    }
    let Some(input) = value.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    input.retain(|item| item["type"] != "configuration_update");
    for item in input {
        let kind = item["type"].as_str().unwrap_or("").to_owned();
        if let Some(object) = item.as_object_mut() {
            object.remove("internal_chat_message_metadata_passthrough");
            // These accompany plaintext replay; reasoning/compaction ciphertext
            // remains untouched unless this upstream explicitly rejects it.
            if matches!(
                kind.as_str(),
                "message"
                    | "function_call"
                    | "custom_tool_call"
                    | "function_call_output"
                    | "custom_tool_call_output"
            ) {
                object.remove("encrypted_content");
            }
            if kind == "function_call" {
                object.remove("encrypted_function_args");
            }
        }
        if kind == "function_call" {
            if let Some(args) = item["arguments"].as_str() {
                if serde_json::from_str::<DuplicateKeys>(args).is_ok_and(|v| v.0) {
                    if let Ok(parsed) = serde_json::from_str::<Value>(args) {
                        item["arguments"] = Value::String(parsed.to_string());
                    }
                }
            }
        }
        if matches!(
            kind.as_str(),
            "function_call_output" | "custom_tool_call_output"
        ) && item
            .get("call_id")
            .is_none_or(|v| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()))
        {
            if let Some(content) = receipt_content(&item["output"]) {
                *item = json!({"role":"user", "content":content});
            }
        }
    }
}

fn receipt_content(output: &Value) -> Option<Vec<Value>> {
    if let Some(text) = output.as_str() {
        return Some(vec![json!({"type":"input_text","text":text})]);
    }
    output
        .as_array()?
        .iter()
        .map(|part| {
            let mut part = part.clone();
            match part["type"].as_str()? {
                "text" | "output_text" if part["text"].is_string() => {
                    part["type"] = json!("input_text")
                }
                "input_text" | "input_image" | "input_file" => {}
                _ => return None,
            }
            Some(part)
        })
        .collect()
}

// Inspect keys separately so well-formed argument strings retain their exact
// formatting and cache prefix. serde_json's Value parser supplies last-key wins.
struct DuplicateKeys(bool);
impl<'de> Deserialize<'de> for DuplicateKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = DuplicateKeys;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut keys = std::collections::HashSet::new();
                let mut repeated = false;
                while let Some(key) = map.next_key::<String>()? {
                    repeated |= !keys.insert(key);
                    repeated |= map.next_value::<DuplicateKeys>()?.0;
                }
                Ok(DuplicateKeys(repeated))
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut repeated = false;
                while let Some(v) = seq.next_element::<DuplicateKeys>()? {
                    repeated |= v.0;
                }
                Ok(DuplicateKeys(repeated))
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(DuplicateKeys(false))
            }
        }
        d.deserialize_any(Check)
    }
}

/// A retry is justified only by a precise upstream rejection, and only if the
/// remaining plaintext contains the conversation/task. Never erase opaque
/// compaction or an encrypted agent task to make a request appear successful.
pub(crate) fn recover_history(body: &[u8], error: &str) -> Option<bytes::Bytes> {
    let message = error.to_ascii_lowercase();
    let foreign_ids =
        message.contains("requested item was created under a different azure openai resource");
    let rejected = foreign_ids
        || message.contains("invalid_encrypted_content")
        || message.contains("encrypted function output content could not be decrypted or decoded")
        || message.contains("encrypted content could not be verified");
    if !rejected {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let response = if value["response"].is_object() {
        &mut value["response"]
    } else {
        &mut value
    };
    if response
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return None;
    }
    let items = response.get_mut("input")?.as_array_mut()?;
    if items.iter().any(|item| {
        matches!(
            item["type"].as_str(),
            Some("compaction" | "compaction_summary" | "agent_message")
        ) && has_ciphertext(item)
    }) {
        return None;
    }
    let before = items.clone();
    for item in items.iter_mut() {
        if foreign_ids {
            if let Some(object) = item.as_object_mut() {
                object.remove("id");
            }
        }
        if matches!(
            item["type"].as_str(),
            Some("function_call_output" | "custom_tool_call_output")
        ) {
            if let Some(parts) = item.get_mut("output").and_then(Value::as_array_mut) {
                if parts.iter().any(has_ciphertext) {
                    // No plaintext result means there is nothing safe to replay.
                    if !parts.iter().any(|p| {
                        !has_ciphertext(p)
                            && p.get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|s| !s.is_empty())
                    }) {
                        return None;
                    }
                    for part in parts {
                        if has_ciphertext(part) {
                            *part = json!({"type":"input_text", "text":"[Unavailable encrypted historical content]"});
                        }
                    }
                }
            }
        }
    }
    items.retain(|item| item["type"] != "reasoning" || !has_ciphertext(item));
    if *items == before || items.is_empty() {
        return None;
    }
    Some(bytes::Bytes::from(value.to_string()))
}
fn has_ciphertext(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("encrypted_content")
                || value["type"] == "encrypted_text"
                || object.values().any(has_ciphertext)
        }
        Value::Array(items) => items.iter().any(has_ciphertext),
        _ => false,
    }
}
