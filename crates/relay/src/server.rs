use crate::content_encoding::MAX_REQUEST_BODY_BYTES;
use crate::proxy::proxy;
use crate::state::RelayState;
use crate::websocket::responses_websocket;
use axum::{
    extract::DefaultBodyLimit,
    routing::{any, get, post},
    Router,
};
use codex_companion_core::{ConfigStore, RelayConfig};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};
use url::{Host, Url};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStartOutcome {
    pub bind_addr: String,
    pub base_url: String,
}

pub struct BoundRelay {
    store: ConfigStore,
    listener: tokio::net::TcpListener,
    outcome: RelayStartOutcome,
    enforce_api_key: bool,
}

impl BoundRelay {
    pub async fn bind(store: ConfigStore) -> anyhow::Result<Self> {
        let config = store.load()?;
        let bind_addr = config.relay.bind_addr();
        let base_url = config.relay.base_url();
        let addr: SocketAddr = bind_addr.parse()?;
        validate_relay_bind_security(&config.relay, addr)?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let enforce_api_key = !addr.ip().is_loopback();
        Ok(Self {
            store,
            listener,
            outcome: RelayStartOutcome {
                bind_addr,
                base_url,
            },
            enforce_api_key,
        })
    }

    pub fn outcome(&self) -> RelayStartOutcome {
        self.outcome.clone()
    }

    pub async fn serve(self) -> anyhow::Result<RelayStartOutcome> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        let state = RelayState::new_with_api_key_floor(self.store, client, self.enforce_api_key);
        let app = relay_router(state);
        axum::serve(self.listener, app).await?;
        Ok(self.outcome)
    }
}

pub async fn serve(store: ConfigStore) -> anyhow::Result<RelayStartOutcome> {
    BoundRelay::bind(store).await?.serve().await
}

fn relay_router(state: RelayState) -> Router {
    relay_router_with_body_limit(state, MAX_REQUEST_BODY_BYTES)
}

fn relay_router_with_body_limit(state: RelayState, body_limit: usize) -> Router {
    Router::new()
        .route("/v1/responses", get(responses_websocket).post(proxy))
        .route("/v1/responses/compact", post(proxy))
        .route("/{*path}", any(proxy))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|origin, _request| {
                    browser_origin_is_loopback(origin)
                }))
                .allow_methods(Any)
                // `Authorization` is a CORS non-wildcard header, so `*` does not
                // authorize it in browsers. Echo the requested header names for
                // origins that passed the loopback predicate.
                .allow_headers(AllowHeaders::mirror_request()),
        )
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

fn validate_relay_bind_security(relay: &RelayConfig, addr: SocketAddr) -> anyhow::Result<()> {
    if !addr.ip().is_loopback() && !relay.require_api_key {
        anyhow::bail!(
            "refusing unauthenticated non-loopback relay bind at {addr}; enable require_api_key or bind to loopback"
        );
    }
    Ok(())
}

fn browser_origin_is_loopback(origin: &axum::http::HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = Url::parse(origin) else {
        return false;
    };
    match origin.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, StatusCode};
    use bytes::Bytes;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, ProviderConfig, ProviderKind, RelayConfig,
        DEFAULT_GROUP_ID,
    };
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex};

    #[test]
    fn non_loopback_bind_requires_api_key() {
        let mut relay = RelayConfig {
            host: "0.0.0.0".to_string(),
            ..RelayConfig::default()
        };
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), relay.port);

        assert!(validate_relay_bind_security(&relay, addr).is_err());
        relay.require_api_key = true;
        assert!(validate_relay_bind_security(&relay, addr).is_ok());
    }

    #[test]
    fn loopback_bind_does_not_require_api_key() {
        let relay = RelayConfig::default();
        let addr = relay.bind_addr().parse().expect("loopback address");

        assert!(validate_relay_bind_security(&relay, addr).is_ok());
    }

    #[test]
    fn cors_accepts_only_loopback_browser_origins() {
        for origin in [
            "http://localhost:1420",
            "https://localhost",
            "http://127.0.0.1:3000",
            "http://[::1]:5173",
            "tauri://localhost",
        ] {
            let origin = origin.parse().expect("origin header");
            assert!(
                browser_origin_is_loopback(&origin),
                "loopback origin was rejected: {origin:?}"
            );
        }
        for origin in ["https://evil.example", "null", "https://192.168.1.2"] {
            let origin = origin.parse().expect("origin header");
            assert!(
                !browser_origin_is_loopback(&origin),
                "non-loopback origin was accepted: {origin:?}"
            );
        }
    }

    #[tokio::test]
    async fn cors_preflight_echoes_authorization_for_loopback_origins() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let response = reqwest::Client::new()
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{relay_addr}/v1/responses"),
            )
            .header(header::ORIGIN, "http://localhost:1420")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,content-type",
            )
            .send()
            .await
            .expect("preflight");

        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:1420")
        );
        let allowed_headers = response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .and_then(|value| value.to_str().ok())
            .expect("allowed headers")
            .to_ascii_lowercase();
        assert!(allowed_headers.contains("authorization"));
        assert!(allowed_headers.contains("content-type"));
    }

    #[tokio::test]
    async fn serves_group_provider_over_real_http_listener() {
        let upstream = Router::new().route(
            "/{*path}",
            any(|| async { (StatusCode::OK, r#"{"object":"list","data":[]}"#) }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream).await;
        });

        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert(
                    "account-a".to_string(),
                    ProviderConfig {
                        id: "account-a".to_string(),
                        name: "Account A".to_string(),
                        kind: ProviderKind::OpenAiCompatible,
                        base_url: format!("http://{upstream_addr}/v1"),
                        websocket_url: None,
                        auth_ref: None,
                        direct_auth_ref: None,
                        model_map: BTreeMap::new(),
                        priority: 0,
                        enabled: true,
                        refresh_interval_seconds: default_refresh_interval_seconds(),
                        account: None,
                    },
                );
                config
                    .groups
                    .get_mut(DEFAULT_GROUP_ID)
                    .expect("default group")
                    .provider_order = vec!["account-a".to_string()];
                Ok(())
            })
            .expect("config");

        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let response = reqwest::Client::new()
            .get(format!("http://{relay_addr}/v1/models"))
            .header("x-session-id", "network-test")
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-codex-companion-provider")
                .and_then(|value| value.to_str().ok()),
            Some("account-a")
        );
        assert!(response
            .headers()
            .contains_key("x-codex-companion-request-id"));
        assert!(response.text().await.expect("body").contains("\"data\":[]"));
    }

    #[tokio::test]
    async fn accepts_responses_requests_larger_than_axum_default_limit() {
        let upstream = Router::new().route(
            "/{*path}",
            any(|| async { (StatusCode::OK, r#"{"status":"ok"}"#) }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream).await;
        });
        let store = relay_store_for_upstream(format!("http://{upstream_addr}/v1"));

        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let payload = serde_json::json!({
            "model": "gpt-test",
            "input": "x".repeat(2 * 1024 * 1024),
            "stream": false
        });
        let response = reqwest::Client::new()
            .post(format!("http://{relay_addr}/v1/responses"))
            .json(&payload)
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn returns_structured_local_error_when_configured_body_limit_is_exceeded() {
        let store = relay_store_for_upstream("http://127.0.0.1:9/v1".to_string());
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app =
            relay_router_with_body_limit(RelayState::new(store, reqwest::Client::new()), 1024);
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{relay_addr}/v1/responses"))
            .header("content-type", "application/json")
            .body("x".repeat(2048))
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("error json");
        assert_eq!(body["error"]["code"], "local_request_too_large");
        assert!(body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Codex Companion 本地代理")));
    }

    #[tokio::test]
    async fn decodes_zstd_request_before_routing_and_forwarding() {
        let upstream = Router::new().route(
            "/{*path}",
            any(|headers: HeaderMap, body: Bytes| async move {
                let decoded: serde_json::Value =
                    serde_json::from_slice(&body).expect("upstream json");
                assert_eq!(decoded["model"], "gpt-test");
                assert!(!headers.contains_key(header::CONTENT_ENCODING));
                (StatusCode::OK, r#"{"status":"ok"}"#)
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream).await;
        });
        let store = relay_store_for_upstream(format!("http://{upstream_addr}/v1"));

        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let payload = br#"{"model":"gpt-test","input":"hello","stream":false}"#;
        let encoded =
            zstd::stream::encode_all(std::io::Cursor::new(payload), 0).expect("zstd encode");
        let response = reqwest::Client::new()
            .post(format!("http://{relay_addr}/v1/responses"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "zstd")
            .body(encoded)
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sends_official_max_for_ultra_to_native_responses_upstream() {
        let received = Arc::new(Mutex::new(None));
        let received_by_upstream = received.clone();
        let upstream = Router::new().route(
            "/{*path}",
            any(move |body: Bytes| {
                let received = received_by_upstream.clone();
                async move {
                    let payload: serde_json::Value =
                        serde_json::from_slice(&body).expect("upstream request json");
                    *received.lock().expect("received lock") = Some(payload);
                    (StatusCode::OK, r#"{"status":"ok"}"#)
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream).await;
        });
        let store = relay_store_for_upstream(format!("http://{upstream_addr}/v1"));
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{relay_addr}/v1/responses"))
            .json(&serde_json::json!({
                "model": "gpt-5.6-sol",
                "input": "hello",
                "reasoning": { "effort": "ultra" },
                "stream": false
            }))
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = received
            .lock()
            .expect("received lock")
            .clone()
            .expect("captured upstream request");
        assert_eq!(payload["reasoning"]["effort"], "max");
    }

    #[tokio::test]
    async fn sends_official_max_for_ultra_to_chat_completions_upstream() {
        let received = Arc::new(Mutex::new(None));
        let received_by_upstream = received.clone();
        let upstream = Router::new().route(
            "/{*path}",
            any(move |body: Bytes| {
                let received = received_by_upstream.clone();
                async move {
                    let payload: serde_json::Value =
                        serde_json::from_slice(&body).expect("upstream request json");
                    *received.lock().expect("received lock") = Some(payload);
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"id":"chatcmpl-ultra","object":"chat.completion","created":1,"model":"gpt-5.6-sol","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                    )
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream bind");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream).await;
        });
        let store = relay_store_for_upstream(format!("http://{upstream_addr}/v1/chat/completions"));
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay bind");
        let relay_addr = relay_listener.local_addr().expect("relay addr");
        let app = relay_router(RelayState::new(store, reqwest::Client::new()));
        tokio::spawn(async move {
            let _ = axum::serve(relay_listener, app).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{relay_addr}/v1/responses"))
            .json(&serde_json::json!({
                "model": "gpt-5.6-sol",
                "input": "hello",
                "reasoning": { "effort": "ultra" },
                "stream": false
            }))
            .send()
            .await
            .expect("relay request");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = received
            .lock()
            .expect("received lock")
            .clone()
            .expect("captured upstream request");
        assert_eq!(payload["reasoning_effort"], "max");
        assert!(payload.get("reasoning").is_none());
    }

    fn relay_store_for_upstream(base_url: String) -> ConfigStore {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.keep().join("config.json"));
        store
            .update(|config| {
                config.providers.insert(
                    "account-a".to_string(),
                    ProviderConfig {
                        id: "account-a".to_string(),
                        name: "Account A".to_string(),
                        kind: ProviderKind::OpenAiCompatible,
                        base_url,
                        websocket_url: None,
                        auth_ref: None,
                        direct_auth_ref: None,
                        model_map: BTreeMap::new(),
                        priority: 0,
                        enabled: true,
                        refresh_interval_seconds: default_refresh_interval_seconds(),
                        account: None,
                    },
                );
                config
                    .groups
                    .get_mut(DEFAULT_GROUP_ID)
                    .expect("default group")
                    .provider_order = vec!["account-a".to_string()];
                Ok(())
            })
            .expect("config");
        store
    }
}
