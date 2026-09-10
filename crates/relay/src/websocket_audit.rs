use crate::events::append_event;
use crate::proxy::next_request_id;
use crate::state::RelayState;
use crate::{RequestAttemptFinish, RequestAttemptStart, RequestLogFinish, RequestLogStart};
use serde_json::Value;
use std::time::Instant;

/// One audit row per response.create, not per long-lived socket. Only routing
/// metadata is retained; neither request frames nor response bodies are stored.
pub(crate) struct WebSocketRequestAudit {
    state: RelayState,
    id: String,
    started: Instant,
    provider: Option<String>,
    attempts: u16,
    attempt_started: Option<Instant>,
    failure: Option<(Option<u16>, String)>,
    finished: bool,
}

impl WebSocketRequestAudit {
    pub(crate) fn new(state: &RelayState, payload: &Value, client_id: Option<&str>) -> Self {
        let id = next_request_id();
        let payload = payload.get("response").unwrap_or(payload);
        let bounded = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(160).collect::<String>())
        };
        let model = bounded(payload.get("model"));
        let effort = bounded(
            payload
                .pointer("/reasoning/effort")
                .or_else(|| payload.get("reasoning_effort")),
        );
        let tier = bounded(payload.get("service_tier"));
        if let Ok(config) = state.store.load() {
            let _ = state
                .api_service
                .prune_request_logs(config.relay.request_log_retention_days);
        }
        let _ = state.api_service.record_request_start(RequestLogStart {
            request_id: &id,
            method: "WS",
            path: "/v1/responses",
            model: model.as_deref(),
            reasoning_effort: effort.as_deref(),
            service_tier: tier.as_deref(),
            client_id,
        });
        append_event(
            &state.store,
            "request",
            None,
            format!("[{id}] WS /v1/responses"),
        );
        Self {
            state: state.clone(),
            id,
            started: Instant::now(),
            provider: None,
            attempts: 0,
            attempt_started: None,
            failure: None,
            finished: false,
        }
    }

    pub(crate) fn attempt(&mut self, provider: &str, reason: &str) {
        if self.attempt_started.is_some() {
            self.fail(None, "WebSocket 上游重新连接");
        }
        if self.attempts > 0 {
            append_event(
                &self.state.store,
                "fallback",
                self.provider.clone(),
                format!("[{}] WebSocket 重试，目标 Provider {provider}", self.id),
            );
        }
        self.attempts = self.attempts.saturating_add(1);
        self.provider = Some(provider.to_owned());
        self.failure = None;
        self.attempt_started = Some(Instant::now());
        let _ = self
            .state
            .api_service
            .record_request_attempt_start(RequestAttemptStart {
                request_id: &self.id,
                attempt: self.attempts,
                provider_id: provider,
                route_reason: reason,
            });
    }

    fn finish_attempt(&mut self, status: Option<u16>, outcome: &str, error: Option<&str>) {
        if let Some(started) = self.attempt_started.take() {
            let _ = self
                .state
                .api_service
                .record_request_attempt_finish(RequestAttemptFinish {
                    request_id: &self.id,
                    attempt: self.attempts,
                    status_code: status,
                    outcome,
                    latency_ms: elapsed_ms(started),
                    error,
                });
        }
    }

    pub(crate) fn fail(&mut self, status: Option<u16>, detail: &str) {
        self.finish_attempt(status, "failed", Some(detail));
        // Store methods redact and bound the persisted error. Keep this local
        // copy equally bounded because a peer controls the error frame.
        self.failure = Some((
            status,
            codex_companion_core::redact_sensitive_text(detail)
                .chars()
                .take(800)
                .collect(),
        ));
    }

    pub(crate) fn finish(&mut self, status: Option<u16>, outcome: &str, error: Option<&str>) {
        if self.finished {
            return;
        }
        self.finish_attempt(status, outcome, error);
        let _ = self
            .state
            .api_service
            .record_request_finish(RequestLogFinish {
                request_id: &self.id,
                provider_id: self.provider.as_deref(),
                status_code: status,
                outcome,
                attempts: self.attempts,
                latency_ms: elapsed_ms(self.started),
                error,
            });
        let safe_error = error.map(|detail| {
            codex_companion_core::redact_sensitive_text(detail)
                .chars()
                .take(800)
                .collect::<String>()
        });
        append_event(
            &self.state.store,
            if outcome == "succeeded" {
                "stream"
            } else {
                "error"
            },
            self.provider.clone(),
            format!(
                "[{}] WS /v1/responses -> {}{}",
                self.id,
                outcome,
                safe_error
                    .as_deref()
                    .map(|s| format!(": {s}"))
                    .unwrap_or_default()
            ),
        );
        self.finished = true;
    }
}

impl Drop for WebSocketRequestAudit {
    fn drop(&mut self) {
        if !self.finished {
            let (status, detail) = self
                .failure
                .take()
                .unwrap_or((None, "WebSocket 请求未完成即中断或取消".to_owned()));
            self.finish(status, "failed", Some(&detail));
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
