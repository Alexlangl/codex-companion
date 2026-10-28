use crate::upstream::{next_sse_block_boundary, ResponseByteStream, MAX_SSE_FRAME_BYTES};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// Recover only collaboration tools declared by this request. A chained
/// gateway may flatten their namespace, but opaque tool payloads stay intact.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolIdentity {
    names: BTreeSet<String>,
    flat_names: BTreeSet<String>,
}

impl ToolIdentity {
    pub(crate) fn from_request(body: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        let mut context = Self::default();
        context.read_tools(&value);
        context
    }

    fn read_tools(&mut self, value: &Value) {
        for field in ["response", "session"] {
            if let Some(nested) = value.get(field).filter(|v| v.is_object()) {
                self.read_tools(nested);
            }
        }
        for tool in value
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match tool["type"].as_str() {
                Some("namespace") if tool["name"] == "collaboration" => {
                    for child in tool["tools"].as_array().into_iter().flatten() {
                        if matches!(child["type"].as_str(), Some("function" | "custom")) {
                            if let Some(name) = child["name"].as_str().filter(|n| !n.is_empty()) {
                                self.names.insert(name.to_owned());
                            }
                        }
                    }
                }
                Some("function" | "custom") => {
                    if let Some(name) = tool["name"].as_str() {
                        self.flat_names.insert(name.to_owned());
                    }
                }
                _ => {}
            }
        }
        for item in value
            .get("input")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if item["type"] == "additional_tools" {
                self.read_tools(item);
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub(crate) fn restore_json(&self, body: Bytes) -> Bytes {
        if self.is_empty() {
            return body;
        }
        let Ok(mut value) = serde_json::from_slice(&body) else {
            return body;
        };
        if !self.restore(&mut value) {
            return body;
        }
        Bytes::from(value.to_string())
    }

    fn restore_frame(&self, frame: Bytes) -> Bytes {
        let Ok(text) = std::str::from_utf8(&frame) else {
            return frame;
        };
        let lines: Vec<_> = text
            .split(['\r', '\n'])
            .filter(|line| !line.is_empty())
            .collect();
        let data = lines
            .iter()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(mut value) = serde_json::from_str(&data) else {
            return frame;
        };
        if !self.restore(&mut value) {
            return frame;
        }
        let mut result = String::new();
        let mut wrote_data = false;
        for line in lines {
            if line.starts_with("data:") {
                if wrote_data {
                    continue;
                }
                result.push_str("data: ");
                result.push_str(&value.to_string());
                wrote_data = true;
            } else {
                result.push_str(line);
            }
            result.push('\n');
        }
        result.push('\n');
        Bytes::from(result)
    }

    pub(crate) fn restore_stream(self, upstream: ResponseByteStream) -> ResponseByteStream {
        if self.is_empty() {
            return upstream;
        }
        Box::pin(stream::unfold(
            (self, upstream, Vec::new(), false, false),
            |(identity, mut upstream, mut buffer, mut finished, mut opaque)| async move {
                loop {
                    if !opaque {
                        if let Some(boundary) = next_sse_block_boundary(&buffer, finished) {
                            let tail = buffer.split_off(boundary.drain_len);
                            let frame = Bytes::from(std::mem::replace(&mut buffer, tail));
                            let frame = if boundary.block_end < MAX_SSE_FRAME_BYTES {
                                identity.restore_frame(frame)
                            } else {
                                frame
                            };
                            return Some((
                                Ok(frame),
                                (identity, upstream, buffer, finished, opaque),
                            ));
                        }
                        if buffer.len() >= MAX_SSE_FRAME_BYTES {
                            // Keep the existing oversized-frame passthrough contract.
                            opaque = true;
                        }
                    }
                    if (finished || opaque) && !buffer.is_empty() {
                        let frame = Bytes::from(std::mem::take(&mut buffer));
                        let frame = if opaque {
                            frame
                        } else {
                            identity.restore_frame(frame)
                        };
                        return Some((Ok(frame), (identity, upstream, buffer, finished, opaque)));
                    }
                    if finished {
                        return None;
                    }
                    match upstream.next().await {
                        Some(Ok(chunk)) if opaque => {
                            return Some((
                                Ok(chunk),
                                (identity, upstream, buffer, finished, opaque),
                            ))
                        }
                        Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
                        Some(Err(error)) => {
                            // Flush any partial frame before reporting the transport error.
                            let error_stream = stream::once(async move { Err(error) });
                            upstream = Box::pin(error_stream);
                            if !buffer.is_empty() {
                                return Some((
                                    Ok(Bytes::from(std::mem::take(&mut buffer))),
                                    (identity, upstream, buffer, false, true),
                                ));
                            }
                            return upstream
                                .next()
                                .await
                                .map(|error| (error, (identity, upstream, buffer, true, opaque)));
                        }
                        None => finished = true,
                    }
                }
            },
        ))
    }

    pub(crate) fn restore(&self, value: &mut Value) -> bool {
        match value.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") => {
                let Some(name) = value["name"].as_str() else {
                    return false;
                };
                if self.flat_names.contains(name)
                    || value
                        .get("namespace")
                        .and_then(Value::as_str)
                        .is_some_and(|n| {
                            !matches!(n, "" | "collaboration" | "collaboration-optimize")
                        })
                {
                    return false;
                }
                let canonical = [
                    "collaboration-optimize__",
                    "collaboration-optimize.",
                    "collaboration__",
                    "collaboration.",
                ]
                .into_iter()
                .find_map(|prefix| name.strip_prefix(prefix))
                .or_else(|| (value["namespace"] == "collaboration-optimize").then_some(name));
                let Some(name) = canonical
                    .filter(|n| self.names.contains(*n))
                    .map(str::to_owned)
                else {
                    return false;
                };
                if self.flat_names.contains(&format!("collaboration__{name}")) {
                    return false;
                }
                value["name"] = json!(name);
                value["namespace"] = json!("collaboration");
                true
            }
            Some("response.output_item.added" | "response.output_item.done") => {
                value.get_mut("item").is_some_and(|item| self.restore(item))
            }
            Some(
                "response.completed"
                | "response.incomplete"
                | "response.created"
                | "response.in_progress",
            ) => value
                .get_mut("response")
                .is_some_and(|response| self.restore(response)),
            _ if value["object"] == "response" => {
                let mut changed = false;
                for item in value
                    .get_mut("output")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    changed |= self.restore(item);
                }
                changed
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restores_fragmented_sse_and_terminal_response_without_touching_payloads() {
        let identity = ToolIdentity::from_request(json!({"tools":[{
            "type":"namespace","name":"collaboration","tools":[{"type":"function","name":"wait_agent"}]
        }]}).to_string().as_bytes());
        let item = json!({"type":"function_call","name":"collaboration-optimize__wait_agent",
            "arguments":"{\"name\":\"collaboration-optimize__wait_agent\"}","call_id":"fixture"});
        let terminal = json!({"type":"response.completed","response":{"object":"response","status":"completed","output":[item]}});
        let wire = format!("id: fixture\r\nevent: response.output_item.added\r\ndata: {}\r\n\r\n: ping\n\ndata: {}\n\n",
            json!({"type":"response.output_item.added","item":item}), terminal);
        let chunks: Vec<_> = wire
            .as_bytes()
            .chunks(7)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        let mut output = identity.restore_stream(Box::pin(stream::iter(chunks)));
        let mut frames = Vec::new();
        while let Some(frame) = output.next().await {
            frames.push(frame.unwrap());
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1], ": ping\n\n");
        for (frame, field) in [(&frames[0], "/item"), (&frames[2], "/response/output/0")] {
            let text = std::str::from_utf8(frame).unwrap();
            let data = text
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            let value: Value = serde_json::from_str(data).unwrap();
            let restored = value.pointer(field).unwrap();
            assert_eq!(restored["name"], "wait_agent");
            assert_eq!(restored["namespace"], "collaboration");
            assert_eq!(restored["arguments"], item["arguments"]);
            assert_eq!(restored["call_id"], item["call_id"]);
        }
        assert!(std::str::from_utf8(&frames[0])
            .unwrap()
            .starts_with("id: fixture\nevent: response.output_item.added\n"));
    }

    #[test]
    fn restores_declared_collaboration_calls_and_preserves_opaque_payloads() {
        let context = ToolIdentity::from_request(
            json!({"tools":[{
            "type":"namespace","name":"collaboration","tools":[
                {"type":"function","name":"spawn_agent"},
                {"type":"function","name":"wait_agent"}]}]})
            .to_string()
            .as_bytes(),
        );
        for name in [
            "collaboration-optimize__wait_agent",
            "collaboration-optimize.wait_agent",
            "collaboration__wait_agent",
        ] {
            let original = json!({"type":"function_call","name":name,"id":"fc_fixture", "call_id":"call_fixture",
                "arguments":"{\"name\":\"collaboration__opaque\"}","input":{"type":"function_call","name":name}});
            for event in ["response.output_item.added", "response.output_item.done"] {
                let mut value = json!({"type":event,"item":original});
                assert!(context.restore(&mut value));
                assert_eq!(value["item"]["name"], "wait_agent");
                assert_eq!(value["item"]["namespace"], "collaboration");
                for field in ["id", "call_id", "arguments", "input"] {
                    assert_eq!(value["item"][field], original[field]);
                }
            }
            let mut response = json!({"object":"response","output":[original]});
            assert!(context.restore(&mut response));
            assert_eq!(response["output"][0]["name"], "wait_agent");
        }
    }

    #[test]
    fn does_not_rewrite_conflicting_undeclared_or_other_namespace_calls() {
        let context = ToolIdentity::from_request(json!({"tools":[
            {"type":"namespace","name":"collaboration","tools":[{"type":"function","name":"wait_agent"}]},
            {"type":"function","name":"collaboration__wait_agent"}
        ]}).to_string().as_bytes());
        for mut value in [
            json!({"type":"function_call","name":"collaboration__wait_agent"}),
            json!({"type":"function_call","name":"collaboration-optimize__wait_agent"}),
            json!({"type":"function_call","name":"collaboration-optimize__unknown"}),
            json!({"type":"function_call","name":"collaboration-optimize__wait_agent","namespace":"other"}),
            json!({"type":"function_call_output","output":{"type":"function_call","name":"collaboration-optimize__wait_agent"}}),
        ] {
            let original = value.clone();
            assert!(!context.restore(&mut value));
            assert_eq!(value, original);
        }
    }
}
