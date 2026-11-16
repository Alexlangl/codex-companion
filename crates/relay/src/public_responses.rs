use serde_json::Value;

/// Public Responses endpoints reject this Codex backend-only include option.
/// Keep encrypted reasoning history and all public include options intact.
pub(crate) fn normalize(value: &mut Value) {
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
}
