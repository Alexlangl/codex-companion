use crate::content_encoding::MAX_REQUEST_BODY_BYTES;
use crate::proxy::proxy;
use crate::state::RelayState;
use crate::websocket::responses_websocket;
use axum::{
    extract::DefaultBodyLimit,
    routing::{any, get, post},
    Router,
};
use codex_companion_core::ConfigStore;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStartOutcome {
    pub bind_addr: String,
    pub base_url: String,
}

pub async fn serve(store: ConfigStore) -> anyhow::Result<RelayStartOutcome> {
    let config = store.load()?;
    let bind_addr = config.relay.bind_addr();
    let base_url = config.relay.base_url();
    let addr: SocketAddr = bind_addr.parse()?;
    let state = RelayState::new(store, reqwest::Client::new());
    let app = relay_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(RelayStartOutcome {
        bind_addr,
        base_url,
    })
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
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, StatusCode};
    use bytes::Bytes;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, ProviderConfig, ProviderKind,
        DEFAULT_GROUP_ID,
    };
    use std::collections::BTreeMap;

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
