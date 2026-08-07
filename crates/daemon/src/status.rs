use crate::launch::{relay_model_slugs, relay_official_auth_provider};
use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CompanionConfig, CompanionStatus, DataRootStatus, ProviderConfig,
    ProviderGroup, RelayEvent, Result,
};
use codex_companion_health::repair_legacy_auth_misclassification;
use codex_companion_provider::{active_group, selected_providers};
use codex_companion_relay::read_recent_events;
use codex_companion_state::{
    doctor, install_companion_provider_for_relay, uninstall_companion_provider,
};
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn status(&self) -> Result<CompanionStatus> {
        let config = self.load_config_with_repaired_legacy_health()?;
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

    fn load_config_with_repaired_legacy_health(&self) -> Result<CompanionConfig> {
        let mut config = self.store.load()?;
        if !repair_legacy_health(&mut config) {
            return Ok(config);
        }
        self.store.update(|current| {
            repair_legacy_health(current);
            Ok(current.clone())
        })
    }

    pub fn install(
        &self,
        codex_dir: Option<PathBuf>,
    ) -> Result<codex_companion_core::CodexInstallStatus> {
        let config = self.store.load()?;
        let selected = selected_providers(&config);
        let models = relay_model_slugs(&selected);
        let official_auth_provider = relay_official_auth_provider(&config, &selected);
        Ok(install_companion_provider_for_relay(
            codex_dir,
            &config.relay,
            None,
            &models,
            official_auth_provider.as_ref(),
            false,
        )?
        .codex)
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

fn repair_legacy_health(config: &mut CompanionConfig) -> bool {
    let mut repaired = false;
    for health in config.health.values_mut() {
        repaired |= repair_legacy_auth_misclassification(health);
    }
    repaired
}

#[allow(dead_code)]
fn _keep_types(_: Option<ProviderGroup>, _: Vec<ProviderConfig>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, HealthFailureKind, HealthStatusKind,
        ProviderHealth, ProviderKind, DEFAULT_GROUP_ID,
    };
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn install_uses_models_declared_by_the_active_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("companion.json"));
        store
            .update(|config| {
                let provider = ProviderConfig {
                    id: "relay".to_string(),
                    name: "Relay".to_string(),
                    kind: ProviderKind::RelayProvider,
                    base_url: "https://relay.example.com/v1".to_string(),
                    websocket_url: None,
                    auth_ref: None,
                    direct_auth_ref: None,
                    model_map: BTreeMap::from([(
                        "gpt-custom".to_string(),
                        "upstream-custom".to_string(),
                    )]),
                    priority: 100,
                    enabled: true,
                    refresh_interval_seconds: default_refresh_interval_seconds(),
                    account: None,
                };
                config
                    .groups
                    .get_mut(DEFAULT_GROUP_ID)
                    .expect("default group")
                    .provider_order
                    .push(provider.id.clone());
                config.providers.insert(provider.id.clone(), provider);
                Ok(())
            })
            .expect("config");
        let codex_dir = temp.path().join("codex");

        CompanionDaemon::new(store)
            .install(Some(codex_dir.clone()))
            .expect("install");

        let config = fs::read_to_string(codex_dir.join("config.toml")).expect("codex config");
        assert!(config.contains("model_catalog_json"));
        let catalog = fs::read_to_string(codex_dir.join("codex-companion-model-catalog.json"))
            .expect("model catalog");
        assert!(catalog.contains("gpt-custom"));
    }

    #[test]
    fn status_config_repairs_and_persists_legacy_balance_auth_failures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("companion.json"));
        store
            .update(|config| {
                config.health.insert(
                    "relay".to_string(),
                    ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        last_error: Some(
                            "上游返回 403: INSUFFICIENT_BALANCE: Insufficient account balance"
                                .to_string(),
                        ),
                        last_failure_kind: Some(HealthFailureKind::AuthFailed),
                        ..ProviderHealth::default()
                    },
                );
                Ok(())
            })
            .expect("seed health");
        let daemon = CompanionDaemon::new(store.clone());

        let config = daemon
            .load_config_with_repaired_legacy_health()
            .expect("load repaired status config");

        assert_eq!(
            config.health["relay"].status,
            HealthStatusKind::QuotaExhausted
        );
        assert_eq!(
            store.load().expect("persisted config").health["relay"].status,
            HealthStatusKind::QuotaExhausted
        );
    }
}
