use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CompanionStatus, DataRootStatus, ProviderConfig, ProviderGroup, RelayEvent,
    Result,
};
use codex_companion_provider::{active_group, selected_providers};
use codex_companion_relay::read_recent_events;
use codex_companion_state::{doctor, install_companion_provider, uninstall_companion_provider};
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn status(&self) -> Result<CompanionStatus> {
        let config = self.store.load()?;
        let codex_dir = default_codex_dir()?;
        let codex = doctor(codex_dir, &config.relay)?;
        Ok(CompanionStatus {
            relay_base_url: config.relay.base_url(),
            active_group: active_group(&config),
            active_providers: selected_providers(&config),
            data_dir: self.store.data_dir(),
            config_path: self.store.path().to_path_buf(),
            config,
            codex,
            recent_events: self.relay_events(),
            data_roots: DataRootStatus {
                companion_isolated: non_empty_env("CODEX_COMPANION_HOME"),
                codex_isolated: non_empty_env("CODEX_COMPANION_CODEX_DIR"),
            },
        })
    }

    pub fn install(
        &self,
        codex_dir: Option<PathBuf>,
    ) -> Result<codex_companion_core::CodexInstallStatus> {
        let config = self.store.load()?;
        install_companion_provider(codex_dir, &config.relay)
    }

    pub fn uninstall(
        &self,
        codex_dir: Option<PathBuf>,
    ) -> Result<codex_companion_core::CodexInstallStatus> {
        uninstall_companion_provider(codex_dir)
    }

    pub fn doctor(
        &self,
        codex_dir: Option<PathBuf>,
    ) -> Result<codex_companion_core::CodexInstallStatus> {
        let config = self.store.load()?;
        doctor(codex_dir.unwrap_or(default_codex_dir()?), &config.relay)
    }

    pub fn relay_events(&self) -> Vec<RelayEvent> {
        read_recent_events(&self.store.data_dir(), 100)
    }
}

fn non_empty_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

#[allow(dead_code)]
fn _keep_types(_: Option<ProviderGroup>, _: Vec<ProviderConfig>) {}
