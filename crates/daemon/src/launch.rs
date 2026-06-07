use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CodexLaunchMode, CodexLaunchOutcome, CompanionError, GroupPolicy,
    ProviderConfig, ProviderGroup, ProviderKind, ProviderLaunchMode, RepairOptions, Result,
    COMPANION_PROVIDER_ID,
};
use codex_companion_provider::use_group;
use codex_companion_state::{install_companion_provider, install_direct_provider, repair_state};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

const SINGLE_PROVIDER_GROUP_PREFIX: &str = "single-";

impl CompanionDaemon {
    pub fn launch_group(
        &self,
        group_id: &str,
        codex_dir: Option<PathBuf>,
    ) -> Result<CodexLaunchOutcome> {
        let group = use_group(&self.store, group_id)?;
        let config = self.store.load()?;
        let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
        let codex = install_companion_provider(Some(codex_dir.clone()), &config.relay)?;
        let repair = repair_state(RepairOptions {
            codex_dir,
            history: true,
            plugins: true,
            dry_run: false,
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
        })?;
        let codex_started = restart_codex();

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::GroupRelay,
            target_id: group.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            codex,
            repair,
            codex_started,
            message: format!(
                "已启动分组 {}，Codex namespace 使用 codex-companion",
                group.name
            ),
        })
    }

    pub fn launch_provider(
        &self,
        provider_id: &str,
        codex_dir: Option<PathBuf>,
    ) -> Result<CodexLaunchOutcome> {
        self.launch_provider_with_mode(provider_id, codex_dir, ProviderLaunchMode::Auto)
    }

    pub fn launch_provider_with_mode(
        &self,
        provider_id: &str,
        codex_dir: Option<PathBuf>,
        mode: ProviderLaunchMode,
    ) -> Result<CodexLaunchOutcome> {
        let provider = self
            .store
            .load()?
            .providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| {
                CompanionError::InvalidConfig(format!("unknown provider: {provider_id}"))
            })?;
        let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);

        let should_direct = match mode {
            ProviderLaunchMode::Direct => {
                if !provider_can_direct_connect(&provider) {
                    return Err(CompanionError::InvalidConfig(format!(
                        "provider {} 缺少直连中转站所需的 API Key",
                        provider.name
                    )));
                }
                true
            }
            ProviderLaunchMode::Relay => false,
            ProviderLaunchMode::Auto => provider_can_direct_connect(&provider),
        };

        if should_direct {
            let codex = install_direct_provider(Some(codex_dir.clone()), &provider)?;
            let repair = repair_state(RepairOptions {
                codex_dir,
                history: true,
                plugins: true,
                dry_run: false,
                target_provider_id: provider.id.clone(),
            })?;
            let codex_started = restart_codex();
            return Ok(CodexLaunchOutcome {
                mode: CodexLaunchMode::ProviderDirect,
                target_id: provider.id.clone(),
                target_provider_id: provider.id.clone(),
                codex,
                repair,
                codex_started,
                message: format!("已直连启动 provider {}", provider.name),
            });
        }

        self.ensure_single_provider_group(&provider)?;
        let config = self.store.load()?;
        let codex = install_companion_provider(Some(codex_dir.clone()), &config.relay)?;
        let repair = repair_state(RepairOptions {
            codex_dir,
            history: true,
            plugins: true,
            dry_run: false,
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
        })?;
        let codex_started = restart_codex();

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::ProviderRelay,
            target_id: provider.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            codex,
            repair,
            codex_started,
            message: format!(
                "已通过 relay 启动 provider {}，原因：{}",
                provider.name,
                provider_relay_reason(&provider)
            ),
        })
    }

    fn ensure_single_provider_group(&self, provider: &ProviderConfig) -> Result<ProviderGroup> {
        let group_id = single_provider_group_id(provider);
        self.store.update(|config| {
            let group = ProviderGroup {
                id: group_id.clone(),
                name: format!("{} 单 Provider", provider.name),
                policy: GroupPolicy::Manual,
                provider_order: vec![provider.id.clone()],
                fallback_enabled: false,
            };
            config.groups.insert(group_id.clone(), group.clone());
            config.relay.active_group_id = group_id;
            Ok(group)
        })
    }
}

pub fn provider_can_direct_connect(provider: &ProviderConfig) -> bool {
    if matches!(provider.kind, ProviderKind::OfficialCodex) {
        return false;
    }
    provider
        .direct_auth_ref
        .as_deref()
        .or(provider.auth_ref.as_deref())
        .is_none_or(|auth_ref| {
            let auth_ref = auth_ref.trim();
            auth_ref.is_empty() || auth_ref.starts_with("env:") || auth_ref.starts_with("file:")
        })
}

pub fn provider_relay_reason(provider: &ProviderConfig) -> &'static str {
    if matches!(provider.kind, ProviderKind::OfficialCodex) {
        "官方账号需要 Companion 续期 OAuth token 并注入 Codex headers"
    } else if provider
        .auth_ref
        .as_deref()
        .is_some_and(|auth_ref| auth_ref.starts_with("file:"))
    {
        "密钥保存在 Companion auth 文件中，需要 relay 注入 Authorization"
    } else {
        "该 provider 需要 Companion relay 能力"
    }
}

pub fn single_provider_group_id(provider: &ProviderConfig) -> String {
    format!("{SINGLE_PROVIDER_GROUP_PREFIX}{}", provider.id)
}

fn restart_codex() -> bool {
    stop_codex();
    thread::sleep(Duration::from_millis(650));
    start_codex()
}

#[cfg(target_os = "macos")]
fn stop_codex() {
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "Codex" to quit"#)
        .status();
    let _ = Command::new("pkill").args(["-x", "Codex"]).status();
}

#[cfg(target_os = "windows")]
fn stop_codex() {
    let _ = Command::new("taskkill")
        .args(["/IM", "Codex.exe", "/F"])
        .status();
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn stop_codex() {
    let _ = Command::new("pkill").arg("codex").status();
}

#[cfg(target_os = "macos")]
fn start_codex() -> bool {
    Command::new("open")
        .args(["-a", "Codex"])
        .status()
        .is_ok_and(|status| status.success())
        || Command::new("codex").spawn().is_ok()
}

#[cfg(target_os = "windows")]
fn start_codex() -> bool {
    Command::new("cmd")
        .args(["/C", "start", "codex"])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn start_codex() -> bool {
    Command::new("codex").spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::default_refresh_interval_seconds;
    use std::collections::BTreeMap;

    fn provider(kind: ProviderKind, auth_ref: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: "p1".to_string(),
            name: "Provider".to_string(),
            kind,
            base_url: "https://example.com/v1".to_string(),
            auth_ref: auth_ref.map(ToOwned::to_owned),
            direct_auth_ref: auth_ref
                .filter(|auth_ref| auth_ref.starts_with("env:"))
                .map(ToOwned::to_owned),
            model_map: BTreeMap::new(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn official_provider_requires_relay() {
        let provider = provider(ProviderKind::OfficialCodex, Some("env:OPENAI_API_KEY"));
        assert!(!provider_can_direct_connect(&provider));
    }

    #[test]
    fn file_api_key_auth_can_direct_connect() {
        let provider = provider(ProviderKind::OpenAiCompatible, Some("file:/tmp/key.json"));
        assert!(provider_can_direct_connect(&provider));
    }

    #[test]
    fn env_auth_can_direct_connect() {
        let provider = provider(
            ProviderKind::OpenAiCompatible,
            Some("env:OPENROUTER_API_KEY"),
        );
        assert!(provider_can_direct_connect(&provider));
    }
}
