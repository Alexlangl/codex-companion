use crate::health_loop::start_health_refresh_loop;
use codex_companion_core::{ConfigStore, Result};

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
        let config = self.store.load()?;
        let outcome = codex_companion_relay::RelayStartOutcome {
            bind_addr: config.relay.bind_addr(),
            base_url: config.relay.base_url(),
        };
        self.start_health_refresh_loop();
        codex_companion_relay::serve(self.store.clone()).await?;
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
}
