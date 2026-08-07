use crate::health_loop::{start_health_refresh_loop, start_scoped_health_refresh_loop};
use codex_companion_core::{ConfigStore, ProviderRefreshProgress, Result};

#[derive(Debug, Clone)]
pub struct CompanionDaemon {
    pub(crate) store: ConfigStore,
}

impl CompanionDaemon {
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self> {
        Ok(Self {
            store: ConfigStore::default()?,
        })
    }

    pub fn new(store: ConfigStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &ConfigStore {
        &self.store
    }

    pub async fn start_relay(&self) -> anyhow::Result<codex_companion_relay::RelayStartOutcome> {
        let relay = codex_companion_relay::BoundRelay::bind(self.store.clone()).await?;
        let outcome = relay.outcome();
        if let Err(error) = self.reconcile_preserved_official_codex_auth() {
            eprintln!("Codex official login reconciliation failed: {error}");
        }
        let refresh_loop = start_scoped_health_refresh_loop(self.store.clone());
        let serve_result = relay.serve().await;
        if let Some(refresh_loop) = refresh_loop {
            refresh_loop.stop().await;
        }
        serve_result?;
        Ok(outcome)
    }
}

impl CompanionDaemon {
    pub fn start_background_tasks(&self) {
        self.start_health_refresh_loop();
    }

    pub fn start_health_refresh_loop(&self) -> bool {
        start_health_refresh_loop(self.store.clone())
    }

    pub fn provider_refresh_progress(&self) -> ProviderRefreshProgress {
        crate::health_loop::refresh_progress(&self.store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_loop::health_refresh_loop_started;

    #[tokio::test]
    async fn relay_bind_failure_does_not_start_health_refresh_loop() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("occupy port");
        let port = occupied.local_addr().expect("local addr").port();
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.relay.host = "127.0.0.1".to_string();
                config.relay.port = port;
                Ok(())
            })
            .expect("configure relay");
        let daemon = CompanionDaemon::new(store.clone());

        assert!(daemon.start_relay().await.is_err());
        assert!(!health_refresh_loop_started(&store));
    }
}
