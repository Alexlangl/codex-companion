use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CompanionStatus, ProviderConfig, ProviderGroup, RelayEvent, Result,
};
use codex_companion_provider::{active_group, selected_providers};
use codex_companion_state::{doctor, install_companion_provider, uninstall_companion_provider};
use std::{fs, path::PathBuf};

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
            recent_events: recent_events(self.store.data_dir()),
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
}

fn recent_events(data_dir: PathBuf) -> Vec<RelayEvent> {
    let path = data_dir.join("relay").join("events.jsonl");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut events = text
        .lines()
        .rev()
        .take(100)
        .filter_map(|line| serde_json::from_str::<RelayEvent>(line).ok())
        .collect::<Vec<_>>();
    events.reverse();
    events
}

#[allow(dead_code)]
fn _keep_types(_: Option<ProviderGroup>, _: Vec<ProviderConfig>) {}
