use crate::launch::{provider_can_direct_connect, relay_model_slugs, relay_official_auth_provider};
use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CompanionConfig, CompanionStatus, DataRootStatus, ProviderConfig,
    ProviderGroup, RelayEvent, Result,
};
use codex_companion_health::repair_legacy_auth_misclassification;
use codex_companion_provider::{active_group, selected_providers, sync_official_auth_mode};
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
        let direct_connect_provider_ids = config
            .providers
            .values()
            .filter(|provider| provider_can_direct_connect(provider))
            .map(|provider| provider.id.clone())
            .collect();
        Ok(CompanionStatus {
            relay_base_url: config.relay.base_url(),
            active_group: active_group(&config),
            active_providers: selected_providers(&config),
            direct_connect_provider_ids,
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
        let repaired_health = repair_legacy_health(&mut config);
        let synced_auth_mode = sync_official_auth_modes(&mut config);
        if !repaired_health && !synced_auth_mode {
            return Ok(config);
        }
        self.store.update(|current| {
            repair_legacy_health(current);
            sync_official_auth_modes(current);
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

fn sync_official_auth_modes(config: &mut CompanionConfig) -> bool {
    let mut synced = false;
    for provider in config.providers.values_mut() {
        synced |= sync_official_auth_mode(provider);
    }
    synced
}

#[allow(dead_code)]
fn _keep_types(_: Option<ProviderGroup>, _: Vec<ProviderConfig>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, HealthFailureKind, HealthStatusKind,
        ProviderAccountInfo, ProviderHealth, ProviderKind, DEFAULT_GROUP_ID,
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

    #[test]
    fn status_backfills_agent_identity_auth_mode_from_auth_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("companion.json"));
        let auth_path = temp.path().join("agent-auth.json");
        fs::write(&auth_path, r#"{"auth_mode":"agentIdentity"}"#).expect("auth");
        store
            .update(|config| {
                config.providers.insert(
                    "agent".to_string(),
                    ProviderConfig {
                        id: "agent".to_string(),
                        name: "Agent".to_string(),
                        kind: ProviderKind::OfficialCodex,
                        base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                        websocket_url: None,
                        auth_ref: Some(format!("file:{}", auth_path.display())),
                        direct_auth_ref: None,
                        model_map: BTreeMap::new(),
                        priority: 0,
                        enabled: true,
                        refresh_interval_seconds: default_refresh_interval_seconds(),
                        account: None,
                    },
                );
                Ok(())
            })
            .expect("seed config");

        let config = CompanionDaemon::new(store.clone())
            .load_config_with_repaired_legacy_health()
            .expect("status config");

        assert_eq!(
            config.providers["agent"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
        assert_eq!(
            store.load().expect("persisted config").providers["agent"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
    }

    #[test]
    fn status_syncs_file_auth_mode_over_stale_pat_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("companion.json"));
        let auth_path = temp.path().join("oauth-auth.json");
        fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"oauth-access","refresh_token":"oauth-refresh"}}"#,
        )
        .expect("oauth auth");
        store
            .update(|config| {
                config.providers.insert(
                    "oauth".to_string(),
                    ProviderConfig {
                        id: "oauth".to_string(),
                        name: "OAuth".to_string(),
                        kind: ProviderKind::OfficialCodex,
                        base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                        websocket_url: None,
                        auth_ref: Some(format!("file:{}", auth_path.display())),
                        direct_auth_ref: None,
                        model_map: BTreeMap::new(),
                        priority: 0,
                        enabled: true,
                        refresh_interval_seconds: default_refresh_interval_seconds(),
                        account: Some(ProviderAccountInfo {
                            auth_mode: Some("pat".to_string()),
                            ..ProviderAccountInfo::default()
                        }),
                    },
                );
                Ok(())
            })
            .expect("seed config");

        let config = CompanionDaemon::new(store.clone())
            .load_config_with_repaired_legacy_health()
            .expect("status config");

        assert_eq!(
            config.providers["oauth"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("oauth")
        );
        assert_eq!(
            store.load().expect("persisted config").providers["oauth"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("oauth")
        );
    }

    #[test]
    fn status_backfills_auth_modes_for_every_official_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("companion.json"));
        let agent_auth_path = temp.path().join("agent-auth.json");
        let pat_auth_path = temp.path().join("pat-auth.json");
        fs::write(&agent_auth_path, r#"{"auth_mode":"agentIdentity"}"#).expect("agent auth");
        fs::write(
            &pat_auth_path,
            r#"{"codex_companion_auth_mode":"pat","tokens":{"access_token":"pat-token"}}"#,
        )
        .expect("pat auth");
        store
            .update(|config| {
                for (id, auth_path) in [("agent", &agent_auth_path), ("pat", &pat_auth_path)] {
                    config.providers.insert(
                        id.to_string(),
                        ProviderConfig {
                            id: id.to_string(),
                            name: id.to_string(),
                            kind: ProviderKind::OfficialCodex,
                            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                            websocket_url: None,
                            auth_ref: Some(format!("file:{}", auth_path.display())),
                            direct_auth_ref: None,
                            model_map: BTreeMap::new(),
                            priority: 0,
                            enabled: true,
                            refresh_interval_seconds: default_refresh_interval_seconds(),
                            account: None,
                        },
                    );
                }
                Ok(())
            })
            .expect("seed config");

        let config = CompanionDaemon::new(store.clone())
            .load_config_with_repaired_legacy_health()
            .expect("status config");

        assert_eq!(
            config.providers["agent"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
        assert_eq!(
            config.providers["pat"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("pat")
        );
        let persisted = store.load().expect("persisted config");
        assert_eq!(
            persisted.providers["agent"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("agentIdentity")
        );
        assert_eq!(
            persisted.providers["pat"]
                .account
                .as_ref()
                .and_then(|account| account.auth_mode.as_deref()),
            Some("pat")
        );
    }
}
