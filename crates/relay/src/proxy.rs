use crate::events::{append_event, update_health};
use crate::state::RelayState;
use crate::upstream::{send_upstream, stream_response, text_response, upstream_url};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::CompanionConfig;
use codex_companion_health::{
    classify_failure, cooldown_active, mark_failure, mark_success, normalize_expired_cooldown,
};
use codex_companion_provider::selected_providers_for_group;

pub(crate) async fn proxy(
    State(state): State<RelayState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match proxy_inner(state, method, uri, headers, body).await {
        Ok(response) => response,
        Err(message) => text_response(StatusCode::BAD_GATEWAY, message),
    }
}

async fn proxy_inner(
    state: RelayState,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, String> {
    append_event(&state.store, "request", None, format!("{} {}", method, uri));
    if is_relay_root_probe(&method, &uri) {
        return Ok(relay_root_response());
    }
    let mut config = state
        .store
        .load()
        .map_err(|error| format!("failed to load config: {error}"))?;
    normalize_health(&mut config);
    let group = config
        .groups
        .get(&config.relay.active_group_id)
        .cloned()
        .ok_or_else(|| format!("active group not found: {}", config.relay.active_group_id))?;
    let selected = selected_providers_for_group(&config, &group)
        .into_iter()
        .filter(|provider| provider.enabled)
        .collect::<Vec<_>>();
    let should_skip_cooldown = group.fallback_enabled && selected.len() > 1;
    let mut candidates = if should_skip_cooldown {
        selected
            .into_iter()
            .filter(|provider| {
                config
                    .health
                    .get(&provider.id)
                    .is_none_or(|health| !cooldown_active(health))
            })
            .collect::<Vec<_>>()
    } else {
        selected
    };

    if !group.fallback_enabled {
        candidates.truncate(1);
    }
    if candidates.is_empty() {
        let message = "当前本地代理分组没有可用账号".to_string();
        append_event(&state.store, "error", None, message.clone());
        return Ok(text_response(StatusCode::SERVICE_UNAVAILABLE, message));
    }

    let mut last_error = None;
    let candidate_count = candidates.len();
    for (index, provider) in candidates.into_iter().enumerate() {
        let upstream = upstream_url(&provider, &uri);
        match send_upstream(
            &state.client,
            &provider,
            &method,
            &headers,
            body.clone(),
            &upstream,
        )
        .await
        {
            Ok(response) if response.status().is_success() => {
                let status = response.status();
                update_health(&state.store, &provider.id, |health| mark_success(health));
                append_event(
                    &state.store,
                    "stream",
                    Some(provider.id.clone()),
                    format!("{} {} -> {}", method, uri, status),
                );
                return Ok(stream_response(provider.id, response).await);
            }
            Ok(response) => {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                let failure = classify_failure(Some(status.as_u16()), &body_text);
                let message = format!("上游返回 {}: {}", status, compact_error_body(&body_text));
                update_health(&state.store, &provider.id, |health| {
                    mark_failure(health, &failure, message.clone())
                });
                last_error = Some(message.clone());
                let can_retry =
                    failure.retryable && index + 1 < candidate_count && group.fallback_enabled;
                if can_retry {
                    append_event(&state.store, "fallback", Some(provider.id), message);
                    continue;
                }
                append_event(&state.store, "error", Some(provider.id), message);
                return Ok(text_response(status, body_text));
            }
            Err(error) => {
                let failure = classify_failure(None, &error);
                let message = format!("stream 开始前请求失败: {}", compact_error_body(&error));
                update_health(&state.store, &provider.id, |health| {
                    mark_failure(health, &failure, message.clone())
                });
                last_error = Some(message.clone());
                let can_retry =
                    failure.retryable && index + 1 < candidate_count && group.fallback_enabled;
                if can_retry {
                    append_event(&state.store, "fallback", Some(provider.id), message);
                    continue;
                }
                append_event(&state.store, "error", Some(provider.id), message);
            }
        }
    }

    if let Some(error) = last_error.as_ref() {
        append_event(&state.store, "error", None, error.clone());
    }
    Ok(text_response(
        StatusCode::BAD_GATEWAY,
        last_error.unwrap_or_else(|| "all providers failed".to_string()),
    ))
}

fn normalize_health(config: &mut CompanionConfig) {
    for health in config.health.values_mut() {
        normalize_expired_cooldown(health);
    }
}

fn compact_error_body(body: &str) -> String {
    let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 280 {
        format!("{}...", text.chars().take(280).collect::<String>())
    } else if text.is_empty() {
        "无响应正文".to_string()
    } else {
        text
    }
}

fn is_relay_root_probe(method: &Method, uri: &Uri) -> bool {
    method == Method::GET && matches!(uri.path(), "/" | "/v1")
}

fn relay_root_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        )
        .body(Body::from(
            r#"{"object":"codex_companion.relay","status":"ok"}"#,
        ))
        .expect("response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, routing::any, Router};
    use codex_companion_core::{
        ConfigStore, GroupPolicy, ProviderConfig, ProviderGroup, ProviderHealth, ProviderKind,
    };
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn relay_root_probe_is_handled_locally() {
        let root: Uri = "/v1".parse().expect("uri");
        let models: Uri = "/v1/models".parse().expect("uri");

        assert!(is_relay_root_probe(&Method::GET, &root));
        assert!(!is_relay_root_probe(&Method::POST, &root));
        assert!(!is_relay_root_probe(&Method::GET, &models));
    }

    #[tokio::test]
    async fn falls_back_to_next_provider_before_stream_starts() {
        let provider_a_url =
            spawn_mock_server(StatusCode::INTERNAL_SERVER_ERROR, "upstream failed", None).await;
        let provider_b_url = spawn_mock_server(StatusCode::OK, "ok from b", None).await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState {
            store: store.clone(),
            client: reqwest::Client::new(),
        };

        let response = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("b")
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"ok from b");
        let events =
            std::fs::read_to_string(store.data_dir().join("relay/events.jsonl")).expect("events");
        assert!(events.contains("\"kind\":\"fallback\""));
    }

    #[tokio::test]
    async fn success_status_does_not_probe_next_provider() {
        let provider_b_hits = Arc::new(AtomicUsize::new(0));
        let provider_a_url = spawn_mock_server(StatusCode::OK, "stream from a", None).await;
        let provider_b_url = spawn_mock_server(
            StatusCode::OK,
            "should not be called",
            Some(provider_b_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![
            provider("a", &provider_a_url),
            provider("b", &provider_b_url),
        ]);
        let state = RelayState {
            store,
            client: reqwest::Client::new(),
        };

        let response = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("a")
        );
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"stream from a");
        assert_eq!(provider_b_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn single_provider_route_ignores_its_own_cooldown() {
        let provider_hits = Arc::new(AtomicUsize::new(0));
        let provider_url = spawn_mock_server(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporary unavailable",
            Some(provider_hits.clone()),
        )
        .await;
        let store = store_with_group(vec![provider("a", &provider_url)]);
        store
            .update(|config| {
                let failure = classify_failure(Some(503), "temporary unavailable");
                let health = config
                    .health
                    .entry("a".to_string())
                    .or_insert_with(ProviderHealth::default);
                mark_failure(health, &failure, "temporary unavailable".to_string());
                Ok(())
            })
            .expect("seed cooldown");
        let state = RelayState {
            store,
            client: reqwest::Client::new(),
        };

        let response = proxy_inner(
            state,
            Method::GET,
            "/v1/models".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::new(),
        )
        .await
        .expect("proxy");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(provider_hits.load(Ordering::SeqCst), 1);
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(&body[..], b"temporary unavailable");
    }

    async fn spawn_mock_server(
        status: StatusCode,
        body: &'static str,
        hits: Option<Arc<AtomicUsize>>,
    ) -> String {
        let app = Router::new().route(
            "/{*path}",
            any(move || {
                let hits = hits.clone();
                async move {
                    if let Some(hits) = hits {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    fn store_with_group(providers: Vec<ProviderConfig>) -> ConfigStore {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.keep().join("config.json"));
        store
            .update(|config| {
                config.providers = providers
                    .into_iter()
                    .map(|provider| (provider.id.clone(), provider))
                    .collect();
                config.relay.active_group_id = "test".to_string();
                config.groups.insert(
                    "test".to_string(),
                    ProviderGroup {
                        id: "test".to_string(),
                        name: "Test".to_string(),
                        policy: GroupPolicy::PriorityFallback,
                        provider_order: vec!["a".to_string(), "b".to_string()],
                        fallback_enabled: true,
                    },
                );
                Ok(())
            })
            .expect("config");
        store
    }

    fn provider(id: &str, base_url: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.to_string(),
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        }
    }
}
