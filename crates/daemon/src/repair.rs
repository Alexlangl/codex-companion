use crate::launch::{
    direct_repair_target_provider_id, relay_model_slugs, relay_official_auth_provider,
    restart_codex_if_running,
};
use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, AppSettings, CodexLaunchMode, CompanionConfig, CompanionError,
    ProviderLaunchMode, ProviderViewMode, RepairOptions, RepairOutcome, Result, ThemeMode,
    TokenUsageSummary, TokenUsageSyncStatus, COMPANION_PROVIDER_ID,
};
use codex_companion_provider::selected_providers;
use codex_companion_state::{
    collect_token_usage_cached, collect_token_usage_cached_with_filters, doctor,
    install_companion_provider_for_relay, rebuild_token_usage_cached_with_filters,
    relay_preserved_official_auth_is_ready, repair_state, CodexInstallSnapshot,
    TokenUsageDateRange, TokenUsageFilters,
};
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn repair(&self, mut options: RepairOptions) -> Result<RepairOutcome> {
        if options
            .target_provider_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            options.target_provider_id = self.repair_target_provider_id_from_state()?;
        }
        if options.target_provider_id.is_some() {
            let config = self.store.load()?;
            options.target_provider_id = options
                .target_provider_id
                .map(|provider_id| repair_target_provider_id(&config, provider_id));
        }
        repair_state(options)
    }

    pub fn set_theme(&self, theme: ThemeMode) -> Result<ThemeMode> {
        self.store.update(|config| {
            config.app.theme = theme.clone();
            Ok(theme)
        })
    }

    pub fn set_provider_view_mode(&self, mode: ProviderViewMode) -> Result<ProviderViewMode> {
        self.store.update(|config| {
            config.app.provider_view_mode = mode.clone();
            Ok(mode)
        })
    }

    pub fn set_preserve_official_codex_auth(&self, preserve: bool) -> Result<bool> {
        let codex_dir = default_codex_dir()?;
        self.set_preserve_official_codex_auth_in_dir(preserve, codex_dir)
    }

    pub fn reconcile_preserved_official_codex_auth(&self) -> Result<bool> {
        let codex_dir = default_codex_dir()?;
        self.reconcile_preserved_official_codex_auth_in_dir(codex_dir)
    }

    fn reconcile_preserved_official_codex_auth_in_dir(&self, codex_dir: PathBuf) -> Result<bool> {
        let config = self.store.load()?;
        if !config.app.preserve_official_codex_auth {
            return Ok(false);
        }
        let relay_active = matches!(
            config.app.last_codex_launch_mode,
            Some(CodexLaunchMode::GroupRelay | CodexLaunchMode::ProviderRelay)
        ) && doctor(codex_dir.clone(), &config.relay)?.installed;
        if !relay_active || relay_preserved_official_auth_is_ready(&codex_dir)? {
            return Ok(false);
        }
        self.set_preserve_official_codex_auth_in_dir(true, codex_dir)?;
        Ok(true)
    }

    fn set_preserve_official_codex_auth_in_dir(
        &self,
        preserve: bool,
        codex_dir: PathBuf,
    ) -> Result<bool> {
        let config = self.store.load()?;
        let relay_active = matches!(
            config.app.last_codex_launch_mode,
            Some(CodexLaunchMode::GroupRelay | CodexLaunchMode::ProviderRelay)
        ) && doctor(codex_dir.clone(), &config.relay)?.installed;
        let mut install_snapshot = None;
        let relay_reinstalled = if preserve && relay_active {
            let selected = selected_providers(&config);
            let source = relay_official_auth_provider(&config, &selected);
            let models = relay_model_slugs(&selected);
            let snapshot = CodexInstallSnapshot::capture(&codex_dir)?;
            install_companion_provider_for_relay(
                Some(codex_dir.clone()),
                &config.relay,
                Some("Companion relay with preserved official Codex OAuth"),
                &models,
                source.as_ref(),
                true,
            )?;
            install_snapshot = Some(snapshot);
            true
        } else {
            false
        };

        if let Err(error) = self.store.update(|config| {
            config.app.preserve_official_codex_auth = preserve;
            Ok(preserve)
        }) {
            if let Some(snapshot) = install_snapshot {
                return match snapshot.restore() {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(CompanionError::InvalidConfig(format!(
                        "保存官方登录保护设置失败: {error}；恢复 Codex 配置也失败: {rollback_error}"
                    ))),
                };
            }
            return Err(error);
        }
        if relay_reinstalled {
            restart_codex_if_running();
        }
        Ok(preserve)
    }

    pub fn set_token_usage_refresh_interval(&self, seconds: u64) -> Result<u64> {
        if seconds != 0 && !(15..=3600).contains(&seconds) {
            return Err(codex_companion_core::CompanionError::InvalidConfig(
                "Token 自动刷新间隔必须为 0（关闭）或 15-3600 秒".to_string(),
            ));
        }
        self.store.update(|config| {
            config.app.token_usage_refresh_interval_seconds = seconds;
            Ok(seconds)
        })
    }

    pub fn set_provider_launch_mode(
        &self,
        provider_id: String,
        mode: ProviderLaunchMode,
    ) -> Result<ProviderLaunchMode> {
        self.store.update(|config| {
            let previous_mode = config
                .app
                .provider_launch_modes
                .get(&provider_id)
                .cloned()
                .unwrap_or(ProviderLaunchMode::Direct);
            if matches!(previous_mode, ProviderLaunchMode::Direct)
                && matches!(mode, ProviderLaunchMode::Relay)
            {
                config.app.codex_restart_required_on_next_relay = true;
            }
            if matches!(mode, ProviderLaunchMode::Auto) {
                config.app.provider_launch_modes.remove(&provider_id);
            } else {
                config
                    .app
                    .provider_launch_modes
                    .insert(provider_id, mode.clone());
            }
            Ok(mode)
        })
    }

    pub fn reset_app_settings(&self) -> Result<AppSettings> {
        self.store.update(|config| {
            let preserve_official_codex_auth = config.app.preserve_official_codex_auth;
            config.app = AppSettings {
                preserve_official_codex_auth,
                ..AppSettings::default()
            };
            Ok(config.app.clone())
        })
    }

    pub fn token_usage(&self, codex_dir: PathBuf) -> Result<TokenUsageSummary> {
        collect_token_usage_cached(codex_dir, self.store.data_dir().join("cache"))
    }

    pub fn token_usage_in_range(
        &self,
        codex_dir: PathBuf,
        start_date: Option<&str>,
        end_date: Option<&str>,
    ) -> Result<TokenUsageSummary> {
        self.token_usage_filtered(codex_dir, start_date, end_date, None, None, false)
    }

    pub fn token_usage_filtered(
        &self,
        codex_dir: PathBuf,
        start_date: Option<&str>,
        end_date: Option<&str>,
        provider_id: Option<&str>,
        model: Option<&str>,
        rebuild: bool,
    ) -> Result<TokenUsageSummary> {
        let date_range = TokenUsageDateRange::parse(start_date, end_date)?;
        let filters = TokenUsageFilters::parse(provider_id, model);
        let collect = if rebuild {
            rebuild_token_usage_cached_with_filters
        } else {
            collect_token_usage_cached_with_filters
        };
        collect(
            codex_dir,
            self.store.data_dir().join("cache"),
            &date_range,
            &filters,
        )
    }

    pub fn token_usage_sync_status(&self) -> TokenUsageSyncStatus {
        codex_companion_state::token_usage_sync_status()
    }

    fn repair_target_provider_id_from_state(&self) -> Result<Option<String>> {
        let config = self.store.load()?;
        let target = match config.app.last_codex_launch_mode {
            Some(CodexLaunchMode::ProviderDirect) => config
                .app
                .last_codex_target_provider_id
                .clone()
                .map(|provider_id| repair_target_provider_id(&config, provider_id)),
            Some(CodexLaunchMode::GroupRelay) | Some(CodexLaunchMode::ProviderRelay) => {
                Some(COMPANION_PROVIDER_ID.to_string())
            }
            None => None,
        };
        Ok(target)
    }
}

fn repair_target_provider_id(config: &CompanionConfig, provider_id: String) -> String {
    let provider_id = provider_id.trim().to_string();
    config
        .providers
        .get(&provider_id)
        .map(direct_repair_target_provider_id)
        .unwrap_or(provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, ProviderConfig, ProviderKind,
    };
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn provider(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            id: "official-account".to_string(),
            name: "Official".to_string(),
            kind,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 50,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn repair_target_normalizes_official_provider_to_codex_namespace() {
        let mut config = CompanionConfig::default();
        config.providers.insert(
            "official-account".to_string(),
            provider(ProviderKind::OfficialCodex),
        );

        assert_eq!(
            repair_target_provider_id(&config, "official-account".to_string()),
            "openai"
        );
    }

    #[test]
    fn repair_target_keeps_unknown_bucket_names() {
        let config = CompanionConfig::default();

        assert_eq!(
            repair_target_provider_id(&config, "cc-switch-bucket".to_string()),
            "cc-switch-bucket"
        );
    }
    #[test]
    fn reset_app_settings_preserves_official_auth_protection() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("codex-companion-daemon-reset-test-{suffix}"));
        std::fs::create_dir_all(&temp).expect("tempdir");
        let daemon = CompanionDaemon::new(ConfigStore::new(temp.join("config.json")));

        daemon
            .set_preserve_official_codex_auth(true)
            .expect("set preserve");
        daemon.set_theme(ThemeMode::Dark).expect("set theme");

        let settings = daemon.reset_app_settings().expect("reset settings");

        assert!(settings.preserve_official_codex_auth);
        assert_eq!(settings.theme, ThemeMode::Light);
        assert!(
            daemon
                .store()
                .load()
                .expect("stored config")
                .app
                .preserve_official_codex_auth
        );
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn startup_reconciliation_repairs_an_existing_preserved_relay() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_dir = temp.path().join("codex");
        std::fs::create_dir_all(&codex_dir).expect("codex dir");
        std::fs::write(
            codex_dir.join("auth.json"),
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-third-party"}"#,
        )
        .expect("api key auth");
        let source_path = temp.path().join("official-auth.json");
        std::fs::write(
            &source_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "official-access",
                    "refresh_token": "official-refresh"
                }
            })
            .to_string(),
        )
        .expect("official auth");
        let mut official = provider(ProviderKind::OfficialCodex);
        official.auth_ref = Some(format!("file:{}", source_path.display()));
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config
                    .providers
                    .insert(official.id.clone(), official.clone());
                config.app.last_codex_launch_mode = Some(CodexLaunchMode::GroupRelay);
                config.app.preserve_official_codex_auth = true;
                Ok(())
            })
            .expect("config");
        install_companion_provider_for_relay(
            Some(codex_dir.clone()),
            &codex_companion_core::RelayConfig::default(),
            None,
            &[],
            None,
            false,
        )
        .expect("install relay");
        let daemon = CompanionDaemon::new(store);

        assert!(daemon
            .reconcile_preserved_official_codex_auth_in_dir(codex_dir.clone())
            .expect("reconcile protection"));

        let auth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(codex_dir.join("auth.json")).expect("live auth"))
                .expect("auth json");
        assert_eq!(auth["auth_mode"], "chatgpt");
        assert_eq!(auth["OPENAI_API_KEY"], serde_json::Value::Null);
        assert_eq!(auth["tokens"]["access_token"], "official-access");
        let codex_config =
            std::fs::read_to_string(codex_dir.join("config.toml")).expect("live Codex config");
        assert!(codex_config.contains("requires_openai_auth = true"));
        assert!(codex_config.contains("experimental_bearer_token = \"CODEX_COMPANION_RELAY\""));
        assert!(codex_config.contains("show-ultra-in-model-picker-slider = true"));
        assert!(!daemon
            .reconcile_preserved_official_codex_auth_in_dir(codex_dir.clone())
            .expect("repeated reconciliation"));
        assert!(
            daemon
                .store()
                .load()
                .expect("stored config")
                .app
                .preserve_official_codex_auth
        );
    }

    #[test]
    fn enabling_auth_protection_does_not_restore_oauth_for_a_stale_relay_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_dir = temp.path().join("codex");
        std::fs::create_dir_all(&codex_dir).expect("codex dir");
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"model_provider = "other"

[model_providers.other]
name = "Other"
base_url = "https://example.com/v1"
wire_api = "responses"
"#,
        )
        .expect("other provider config");
        std::fs::write(
            codex_dir.join("auth.json"),
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-third-party"}"#,
        )
        .expect("api key auth");
        let source_path = temp.path().join("official-auth.json");
        std::fs::write(
            &source_path,
            serde_json::json!({
                "tokens": {
                    "access_token": "official-access",
                    "refresh_token": "official-refresh"
                }
            })
            .to_string(),
        )
        .expect("official auth");
        let mut official = provider(ProviderKind::OfficialCodex);
        official.auth_ref = Some(format!("file:{}", source_path.display()));
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert(official.id.clone(), official);
                config.app.last_codex_launch_mode = Some(CodexLaunchMode::GroupRelay);
                Ok(())
            })
            .expect("config");
        let daemon = CompanionDaemon::new(store);
        let config_before = std::fs::read(codex_dir.join("config.toml")).expect("config before");
        let auth_before = std::fs::read(codex_dir.join("auth.json")).expect("auth before");

        daemon
            .set_preserve_official_codex_auth_in_dir(true, codex_dir.clone())
            .expect("enable protection");

        assert_eq!(
            std::fs::read(codex_dir.join("config.toml")).expect("config after"),
            config_before
        );
        assert_eq!(
            std::fs::read(codex_dir.join("auth.json")).expect("auth after"),
            auth_before
        );
        assert!(
            daemon
                .store()
                .load()
                .expect("stored config")
                .app
                .preserve_official_codex_auth
        );
    }
}
