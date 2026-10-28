use crate::upstream::next_sse_block_boundary;
use axum::{body::Body, http::header, response::Response};
use codex_companion_core::{append_diagnostic_log, record_usage_attribution, ConfigStore};
use futures_util::StreamExt;
use serde_json::Value;

const MAX_FRAME_BYTES: usize = crate::upstream::MAX_SSE_FRAME_BYTES;

pub(crate) struct UsageCapture {
    store: ConfigStore,
    request: String,
    session: String,
    provider: String,
    sse: bool,
    buffer: Vec<u8>,
    done: bool,
    terminal: bool,
    finished: bool,
    track_lifecycle: bool,
}

impl UsageCapture {
    pub(crate) fn new(
        store: ConfigStore,
        request: String,
        session: String,
        provider: String,
        sse: bool,
    ) -> Self {
        Self {
            store,
            request,
            session,
            provider,
            sse,
            buffer: Vec::new(),
            done: false,
            terminal: false,
            finished: false,
            track_lifecycle: false,
        }
    }

    pub(crate) fn observe(&mut self, value: &Value) {
        let response = value.get("response").unwrap_or(value);
        if let Some(tier) = response.get("service_tier").and_then(Value::as_str) {
            let _ = crate::ApiServiceStore::from_config_store(&self.store)
                .record_actual_service_tier(&self.request, tier);
        }
        if matches!(
            value["type"].as_str(),
            Some("response.completed" | "response.incomplete" | "response.failed" | "error")
        ) || matches!(
            response["status"].as_str(),
            Some("completed" | "incomplete" | "failed" | "cancelled")
        ) {
            self.terminal = true;
        }
        if self.done {
            return;
        }
        if !matches!(
            response.get("status").and_then(Value::as_str),
            Some("completed" | "incomplete")
        ) {
            return;
        }
        self.done = true;
        if self.session.is_empty() {
            return;
        }
        if let Err(error) = record_usage_attribution(
            &self.store.data_dir(),
            &self.request,
            &self.session,
            &self.provider,
            value,
        ) {
            let _ =
                append_diagnostic_log(&self.store.data_dir(), "warn", "usage", &error.to_string());
        }
    }

    fn finish(&mut self) {
        self.finished = true;
        if self.done || !self.sse {
            return;
        }
        let data = String::from_utf8_lossy(&self.buffer)
            .split(['\r', '\n'])
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(value) = serde_json::from_str(&data) {
            self.observe(&value);
        }
        self.buffer.clear();
    }

    fn push(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8192) {
            if self.done {
                return;
            }
            if self.buffer.len() + chunk.len() > MAX_FRAME_BYTES {
                self.buffer.clear();
                self.done = true;
                return;
            }
            self.buffer.extend_from_slice(chunk);
            if !self.sse
                && self
                    .buffer
                    .iter()
                    .find(|b| !b.is_ascii_whitespace())
                    .is_some_and(|b| matches!(*b, b'd' | b'e' | b':'))
            {
                self.sse = true;
            }
            if self.sse {
                while let Some(boundary) = next_sse_block_boundary(&self.buffer, false) {
                    let data = String::from_utf8_lossy(&self.buffer[..boundary.block_end])
                        .split(['\r', '\n'])
                        .filter_map(|line| line.strip_prefix("data:"))
                        .map(str::trim_start)
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.buffer.drain(..boundary.drain_len);
                    if let Ok(value) = serde_json::from_str(&data) {
                        self.observe(&value);
                    }
                    if self.done {
                        self.buffer.clear();
                        return;
                    }
                }
            } else if let Ok(value) = serde_json::from_slice::<Value>(&self.buffer) {
                self.observe(&value);
                self.buffer.clear();
            }
        }
    }
}

impl Drop for UsageCapture {
    fn drop(&mut self) {
        if self.track_lifecycle && !self.finished && !self.terminal {
            let _ = crate::ApiServiceStore::from_config_store(&self.store).record_stream_outcome(
                &self.request,
                "cancelled",
                Some("client_cancelled"),
            );
        }
    }
}

/// Observe the converted downstream body without changing forwarded bytes.
pub(crate) fn capture_usage(
    response: Response,
    store: ConfigStore,
    request: &str,
    session: Option<&str>,
    provider: &str,
) -> Response {
    let session = session.unwrap_or("");
    // Compressed bytes cannot be inspected without changing the transport.
    if response
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
    {
        return response;
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let sse = content_type.contains("text/event-stream");
    let mut capture =
        UsageCapture::new(store, request.into(), session.into(), provider.into(), sse);
    capture.track_lifecycle = true;
    let (parts, body) = response.into_parts();
    let stream = futures_util::stream::unfold(
        (body.into_data_stream(), capture),
        |(mut body, mut capture)| async move {
            match body.next().await {
                Some(chunk) => {
                    match &chunk {
                        Ok(bytes) => capture.push(bytes),
                        Err(_) => {
                            capture.finished = true;
                            let _ = crate::ApiServiceStore::from_config_store(&capture.store)
                                .record_stream_outcome(
                                    &capture.request,
                                    "failed",
                                    Some("upstream_stream_read_failed"),
                                );
                            capture.done = true;
                            capture.buffer.clear();
                        }
                    }
                    Some((chunk, (body, capture)))
                }
                None => {
                    capture.finish();
                    None
                }
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{apply_usage_attribution, TokenUsageEvent};
    #[test]
    fn fragmented_sse_and_json_record_only_terminal_token_counters() {
        for ending in [None, Some("\r\n\r\n"), Some("\r\r"), Some("")] {
            let sse = ending.is_some();
            let dir = tempfile::tempdir().unwrap();
            let store = ConfigStore::new(dir.path().join("config.json"));
            let data_dir = store.data_dir();
            let mut capture = UsageCapture::new(
                store,
                "request".into(),
                "session".into(),
                "custom-provider".into(),
                sse,
            );
            let json = r#"{"type":"response.completed","response":{"status":"completed","output":[{"text":"private response"}],"usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":80}}}}"#;
            let data = if sse {
                format!(
                    "event: response.completed\r\ndata: {json}{}",
                    ending.unwrap()
                )
            } else {
                json.into()
            };
            for bytes in data.as_bytes().chunks(7) {
                capture.push(bytes);
            }
            capture.finish();
            let mut events = vec![TokenUsageEvent {
                session_id: Some("session".into()),
                timestamp: Some(chrono::Utc::now().to_rfc3339()),
                input_tokens: 20,
                cached_input_tokens: 80,
                output_tokens: 20,
                ..Default::default()
            }];
            apply_usage_attribution(&data_dir, &mut events).unwrap();
            assert_eq!(events[0].provider_id.as_deref(), Some("custom-provider"));
            let bytes = std::fs::read(data_dir.join("usage-attribution.sqlite3")).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private response"));
        }
    }
    #[tokio::test]
    async fn observation_preserves_forwarded_body() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.json"));
        let data = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
        let response = Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(data))
            .unwrap();
        let observed = capture_usage(response, store, "r", Some("s"), "p");
        let bytes = axum::body::to_bytes(observed.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), data.as_bytes());
        assert!(!dir.path().join("usage-attribution.sqlite3").exists());
    }
}
