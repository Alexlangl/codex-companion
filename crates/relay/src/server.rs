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
    let state = RelayState {
        store,
        client: reqwest::Client::new(),
    };
    let app = Router::new()
        .route("/{*path}", any(proxy))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(RelayStartOutcome {
        bind_addr,
        base_url,
    })
}
