use crate::proxy::proxy;
use crate::state::RelayState;
use axum::{routing::any, Router};
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
    Router::new()
        .route("/{*path}", any(proxy))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
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
}
