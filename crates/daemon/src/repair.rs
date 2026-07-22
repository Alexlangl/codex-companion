use crate::launch::direct_repair_target_provider_id;
use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    AppSettings, CodexLaunchMode, CompanionConfig, ProviderLaunchMode, ProviderViewMode,
    RepairOptions, RepairOutcome, Result, ThemeMode, TokenUsageSummary, TokenUsageSyncStatus,
    COMPANION_PROVIDER_ID,
};
use codex_companion_state::{
    collect_token_usage_cached, collect_token_usage_cached_with_filters,
    rebuild_token_usage_cached_with_filters, repair_state, TokenUsageDateRange, TokenUsageFilters,
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
        self.store.update(|config| {
            config.app.preserve_official_codex_auth = preserve;
            Ok(preserve)
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
}
