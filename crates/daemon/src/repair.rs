use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    AppSettings, ProviderLaunchMode, ProviderViewMode, RepairOptions, RepairOutcome, Result,
    ThemeMode, TokenUsageSummary,
};
use codex_companion_state::{collect_token_usage_cached, repair_state};
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn repair(&self, options: RepairOptions) -> Result<RepairOutcome> {
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

    pub fn set_provider_launch_mode(
        &self,
        provider_id: String,
        mode: ProviderLaunchMode,
    ) -> Result<ProviderLaunchMode> {
        self.store.update(|config| {
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
            config.app = AppSettings::default();
            Ok(config.app.clone())
        })
    }

    pub fn token_usage(&self, codex_dir: PathBuf) -> Result<TokenUsageSummary> {
        collect_token_usage_cached(codex_dir, self.store.data_dir().join("cache"))
    }
}
