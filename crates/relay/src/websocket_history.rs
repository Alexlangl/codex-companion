//! Non-generating, acknowledged history chunks for a cold Responses socket.
//! Unknown outcomes must never cause a generating replay on another provider.
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};

pub(super) const MESSAGE_LIMIT: usize = 32 * 1024 * 1024;
const CHUNK_TARGET: usize = 8 * 1024 * 1024;
const EVENT_LIMIT: usize = 4 * 1024 * 1024;

pub(super) struct HistoryPreload {
    envelope: Value,
    chunks: VecDeque<Vec<Value>>,
    wrapped: bool,
    response_id: Option<String>,
    event_bytes: usize,
    events: usize,
}

pub(super) enum Progress {
    Waiting,
    Send { frame: Value, final_chunk: bool },
}

impl HistoryPreload {
    pub(super) fn build(frame: Value) -> Result<Option<Self>, &'static str> {
        Self::with_limits(frame, MESSAGE_LIMIT, CHUNK_TARGET)
    }

    fn with_limits(
        mut frame: Value,
        limit: usize,
        target: usize,
    ) -> Result<Option<Self>, &'static str> {
        if frame["type"] != "response.create" {
            return Ok(None);
        }
        let wrapped = frame["response"].is_object();
        let body = if wrapped {
            &mut frame["response"]
        } else {
            &mut frame
        };
        if body
            .get("previous_response_id")
            .is_some_and(|v| !v.is_null())
            || body["generate"] == false
        {
            return Ok(None);
        }
        let Some(input) = body.get("input").and_then(Value::as_array) else {
            return Ok(None);
        };
        let sizes: Vec<usize> = input
            .iter()
            .map(|item| item.to_string().len() + 1)
            .collect();
        let input = body.as_object_mut().unwrap().remove("input").unwrap();
        let overhead = frame.to_string().len().saturating_add(1024);
        if sizes.iter().fold(overhead, |sum, n| sum.saturating_add(*n)) <= limit {
            return Ok(None);
        }
        let error =
            "历史中单条内容或完整工具调用组超过 WebSocket 预载上限，请使用 HTTP 完整历史请求";
        let capacity = limit.checked_sub(overhead).ok_or(error)?;
        let target = target.saturating_sub(overhead).min(capacity).max(1);
        let mut pending = HashSet::new();
        let mut chunks = VecDeque::new();
        let mut chunk = Vec::new();
        let mut chunk_size = 0usize;
        let mut group = Vec::new();
        let mut group_size = 0usize;
        for (item, size) in input.as_array().unwrap().iter().zip(sizes) {
            match item["type"].as_str() {
                Some("function_call" | "custom_tool_call") => {
                    let id = item["call_id"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .ok_or(error)?;
                    if !pending.insert(id.to_owned()) {
                        return Err(error);
                    }
                }
                Some("function_call_output" | "custom_tool_call_output") => {
                    if let Some(id) = item["call_id"].as_str().filter(|v| !v.is_empty()) {
                        if !pending.remove(id) {
                            return Err(error);
                        }
                    }
                }
                _ => {}
            }
            group.push(item.clone());
            group_size = group_size.saturating_add(size);
            if group_size > capacity {
                return Err(error);
            }
            if pending.is_empty() {
                if !chunk.is_empty() && chunk_size + group_size > target {
                    chunks.push_back(std::mem::take(&mut chunk));
                    chunk_size = 0;
                }
                chunk.append(&mut group);
                chunk_size += group_size;
                group_size = 0;
            }
        }
        if !pending.is_empty() || !group.is_empty() {
            return Err(error);
        }
        if !chunk.is_empty() {
            chunks.push_back(chunk);
        }
        if chunks.len() < 2 {
            return Err(error);
        }
        Ok(Some(Self {
            envelope: frame,
            chunks,
            wrapped,
            response_id: None,
            event_bytes: 0,
            events: 0,
        }))
    }

    pub(super) fn next_frame(&mut self, previous: Option<&str>) -> (Value, bool) {
        let mut frame = self.envelope.clone();
        let body = if self.wrapped {
            &mut frame["response"]
        } else {
            &mut frame
        };
        body["input"] = json!(self.chunks.pop_front().expect("preload chunk"));
        let final_chunk = self.chunks.is_empty();
        if !final_chunk {
            body["generate"] = json!(false);
        }
        if let Some(id) = previous {
            body["previous_response_id"] = json!(id);
        }
        self.response_id = None;
        self.events = 0;
        self.event_bytes = 0;
        (frame, final_chunk)
    }

    pub(super) fn accept(&mut self, bytes: &[u8]) -> Result<Progress, &'static str> {
        let unknown = "上游未确认无生成的历史预载；已停止，未重放请求";
        self.events += 1;
        self.event_bytes = self.event_bytes.saturating_add(bytes.len());
        if self.events > 128 || self.event_bytes > EVENT_LIMIT {
            return Err(unknown);
        }
        let event: Value = serde_json::from_slice(bytes).map_err(|_| unknown)?;
        let response = &event["response"];
        if response
            .get("output")
            .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
            || response
                .pointer("/usage/output_tokens")
                .is_some_and(|v| v.as_u64() != Some(0))
            || response.get("tool_usage").is_some_and(|v| !zero_usage(v))
        {
            return Err(unknown);
        }
        match event["type"].as_str() {
            Some("response.created" | "response.in_progress" | "response.queued") => {
                if let Some(id) = response["id"].as_str() {
                    if self
                        .response_id
                        .as_deref()
                        .is_some_and(|previous| previous != id)
                    {
                        return Err(unknown);
                    }
                    self.response_id = Some(id.to_owned());
                }
                Ok(Progress::Waiting)
            }
            Some("response.completed") => {
                if response["generate"] != false
                    || response["status"] != "completed"
                    || !response["output"].as_array().is_some_and(Vec::is_empty)
                    || !response["error"].is_null()
                {
                    return Err(unknown);
                }
                let id = response["id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 512)
                    .ok_or(unknown)?;
                if self
                    .response_id
                    .as_deref()
                    .is_some_and(|previous| previous != id)
                {
                    return Err(unknown);
                }
                let (frame, final_chunk) = self.next_frame(Some(id));
                Ok(Progress::Send { frame, final_chunk })
            }
            Some("rate_limits.updated") => Ok(Progress::Waiting),
            _ => Err(unknown),
        }
    }
}
fn zero_usage(value: &Value) -> bool {
    value.is_null()
        || value.as_f64() == Some(0.0)
        || value
            .as_object()
            .is_some_and(|v| v.values().all(zero_usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"type":"response.create","model":"gpt-6.1-sol","input":[
        {"role":"user","content":"x".repeat(2000)},
        {"type":"function_call","call_id":"c","name":"read","arguments":"{}"},
        {"type":"function_call_output","call_id":"c","output":"x".repeat(2000)},
        {"role":"user","content":"continue"}]})
    }
    #[test]
    fn chunks_preserve_order_tool_pairs_and_require_acknowledgment() {
        let original = request();
        let mut plan = HistoryPreload::with_limits(original.clone(), 4000, 3500)
            .unwrap()
            .unwrap();
        let (first, final_chunk) = plan.next_frame(None);
        assert!(!final_chunk);
        assert_eq!(first["generate"], false);
        let mut restored = first["input"].as_array().unwrap().clone();
        for index in 0..10 {
            let event = json!({"type":"response.completed","response":{"id":format!("r{index}"),"status":"completed","generate":false,"output":[],"usage":{"output_tokens":0}}});
            let Progress::Send { frame, final_chunk } =
                plan.accept(event.to_string().as_bytes()).unwrap()
            else {
                panic!()
            };
            restored.extend(frame["input"].as_array().unwrap().clone());
            assert_eq!(frame["previous_response_id"], format!("r{index}"));
            if final_chunk {
                assert!(frame.get("generate").is_none());
                assert_eq!(restored, *original["input"].as_array().unwrap());
                return;
            }
            let calls = frame["input"].as_array().unwrap();
            assert_eq!(
                calls
                    .iter()
                    .filter(|v| v["type"] == "function_call")
                    .count(),
                calls
                    .iter()
                    .filter(|v| v["type"] == "function_call_output")
                    .count()
            );
        }
        panic!("preload did not finish");
    }
    #[test]
    fn ambiguous_generation_and_errors_never_advance() {
        for response in [
            json!({"id":"r","status":"completed","output":[]}),
            json!({"id":"r","status":"completed","generate":false,"output":[],"usage":{"output_tokens":1}}),
            json!({"id":"r","status":"completed","generate":false,"output":[{"type":"function_call"}]}),
        ] {
            let mut plan = HistoryPreload::with_limits(request(), 4000, 3500)
                .unwrap()
                .unwrap();
            plan.next_frame(None);
            assert!(plan
                .accept(
                    json!({"type":"response.completed","response":response})
                        .to_string()
                        .as_bytes()
                )
                .is_err());
        }
    }
    #[test]
    fn never_splits_unmatched_calls_or_existing_continuations() {
        let mut value = request();
        value["input"][2]["call_id"] = json!("different");
        assert!(HistoryPreload::with_limits(value.clone(), 4000, 3500).is_err());
        value["previous_response_id"] = json!("existing");
        assert!(HistoryPreload::with_limits(value, 4000, 3500)
            .unwrap()
            .is_none());
    }
}
