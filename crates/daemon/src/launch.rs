use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, CodexLaunchMode, CodexLaunchOutcome, CompanionError, GroupPolicy,
    ProviderConfig, ProviderGroup, ProviderKind, ProviderLaunchMode, RepairOptions, Result,
    COMPANION_PROVIDER_ID,
};
use codex_companion_provider::use_group;
use codex_companion_state::{
    doctor, install_companion_provider, install_direct_provider, repair_state,
};
use std::env;
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
        let previous_config = self.store.load()?;
        let previous_launch_mode = previous_config.app.last_codex_launch_mode.clone();
        let pending_relay_restart = previous_config.app.codex_restart_required_on_next_relay;
        let group = use_group(&self.store, group_id)?;
        let config = self.store.load()?;
        let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
        let relay_installed = doctor(codex_dir.clone(), &config.relay)?.installed;
        let restart_required = relay_restart_required(
            relay_installed,
            previous_launch_mode.as_ref(),
            pending_relay_restart,
        );
        let codex = install_companion_provider(Some(codex_dir.clone()), &config.relay)?;
        let repair = repair_state(RepairOptions {
            codex_dir,
            history: true,
            plugins: true,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })?;
        let codex_launch = ensure_codex_started(restart_required);
        self.record_codex_launch(
            CodexLaunchMode::GroupRelay,
            COMPANION_PROVIDER_ID.to_string(),
        )?;

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::GroupRelay,
            target_id: group.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            codex,
            repair,
            restart_required,
            codex_started: codex_launch.codex_started(),
            message: relay_launch_message("分组", &group.name, restart_required, codex_launch),
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
        let config_snapshot = self.store.load()?;
        let previous_launch_mode = config_snapshot.app.last_codex_launch_mode.clone();
        let pending_relay_restart = config_snapshot.app.codex_restart_required_on_next_relay;
        let provider = config_snapshot
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
                        "provider {} 缺少直连所需的账号材料、API Key 文件或环境变量",
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
                target_provider_id: Some(provider.id.clone()),
            })?;
            let restart_required = true;
            let codex_launch = restart_codex();
            self.record_codex_launch(CodexLaunchMode::ProviderDirect, provider.id.clone())?;
            return Ok(CodexLaunchOutcome {
                mode: CodexLaunchMode::ProviderDirect,
                target_id: provider.id.clone(),
                target_provider_id: provider.id.clone(),
                codex,
                repair,
                restart_required,
                codex_started: codex_launch.codex_started(),
                message: direct_launch_message(&provider.name, codex_launch),
            });
        }

        self.ensure_single_provider_group(&provider)?;
        let config = self.store.load()?;
        let relay_installed = doctor(codex_dir.clone(), &config.relay)?.installed;
        let restart_required = relay_restart_required(
            relay_installed,
            previous_launch_mode.as_ref(),
            pending_relay_restart,
        );
        let codex = install_companion_provider(Some(codex_dir.clone()), &config.relay)?;
        let repair = repair_state(RepairOptions {
            codex_dir,
            history: true,
            plugins: true,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })?;
        let codex_launch = ensure_codex_started(restart_required);
        self.record_codex_launch(
            CodexLaunchMode::ProviderRelay,
            COMPANION_PROVIDER_ID.to_string(),
        )?;

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::ProviderRelay,
            target_id: provider.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            codex,
            repair,
            restart_required,
            codex_started: codex_launch.codex_started(),
            message: format!(
                "{}，原因：{}",
                relay_launch_message("provider", &provider.name, restart_required, codex_launch),
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
            config.health.remove(&provider.id);
            Ok(group)
        })
    }

    fn record_codex_launch(&self, mode: CodexLaunchMode, target_provider_id: String) -> Result<()> {
        self.store.update(|config| {
            config.app.last_codex_launch_mode = Some(mode.clone());
            config.app.last_codex_target_provider_id = Some(target_provider_id);
            config.app.codex_restart_required_on_next_relay = false;
            Ok(())
        })
    }
}

pub fn provider_can_direct_connect(provider: &ProviderConfig) -> bool {
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
        "本地代理由 Companion 续期 OAuth token 并注入 Codex headers"
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

fn relay_restart_required(
    relay_installed: bool,
    previous_launch_mode: Option<&CodexLaunchMode>,
    pending_relay_restart: bool,
) -> bool {
    !relay_installed
        || pending_relay_restart
        || matches!(previous_launch_mode, Some(CodexLaunchMode::ProviderDirect))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexProcessAction {
    None,
    Start,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodexProcessLaunch {
    action: CodexProcessAction,
    succeeded: bool,
    skipped: bool,
}

impl CodexProcessLaunch {
    fn none() -> Self {
        Self {
            action: CodexProcessAction::None,
            succeeded: false,
            skipped: false,
        }
    }

    fn codex_started(self) -> bool {
        !matches!(self.action, CodexProcessAction::None) && self.succeeded
    }
}

fn codex_process_action(restart_required: bool, codex_running: bool) -> CodexProcessAction {
    if restart_required {
        CodexProcessAction::Restart
    } else if codex_running {
        CodexProcessAction::None
    } else {
        CodexProcessAction::Start
    }
}

fn restart_codex() -> CodexProcessLaunch {
    ensure_codex_started(true)
}

fn ensure_codex_started(restart_required: bool) -> CodexProcessLaunch {
    let target = CodexLaunchTarget::from_env();
    let action = codex_process_action(
        restart_required,
        if restart_required {
            true
        } else {
            codex_running(&target)
        },
    );
    if matches!(action, CodexProcessAction::None) {
        return CodexProcessLaunch::none();
    }
    if target.skip_restart {
        return CodexProcessLaunch {
            action,
            succeeded: false,
            skipped: true,
        };
    }
    let succeeded = match action {
        CodexProcessAction::None => false,
        CodexProcessAction::Start => start_codex(&target),
        CodexProcessAction::Restart => {
            stop_codex(&target);
            thread::sleep(Duration::from_millis(650));
            start_codex(&target)
        }
    };
    CodexProcessLaunch {
        action,
        succeeded,
        skipped: false,
    }
}

fn direct_launch_message(provider_name: &str, codex_launch: CodexProcessLaunch) -> String {
    if codex_launch.codex_started() {
        return format!("已直连启动 provider {provider_name}，并已重启 Codex 以载入账号/API Key");
    }
    if codex_launch.skipped {
        return format!(
            "已写入直连 provider {provider_name}；当前配置跳过自动启停，请手动启动/重启 Codex"
        );
    }
    format!("已写入直连 provider {provider_name}；直连模式需要启动/重启 Codex 后才会生效")
}

fn relay_launch_message(
    target_kind: &str,
    target_name: &str,
    restart_required: bool,
    codex_launch: CodexProcessLaunch,
) -> String {
    if restart_required {
        if codex_launch.codex_started() {
            return format!("已通过本地代理启动 {target_kind} {target_name}，并已重启 Codex");
        }
        if codex_launch.skipped {
            return format!(
                "已写入本地代理启动配置；当前配置跳过自动启停，请手动启动/重启 Codex 后使用 {target_kind} {target_name}"
            );
        }
        return format!(
            "已写入本地代理启动配置；请启动/重启 Codex 后使用 {target_kind} {target_name}"
        );
    }

    match codex_launch.action {
        CodexProcessAction::None => {
            format!("已切换本地代理到 {target_kind} {target_name}，Codex 已在本地代理模式运行，无需重启")
        }
        CodexProcessAction::Start if codex_launch.succeeded => {
            format!("已切换本地代理到 {target_kind} {target_name}，并已启动 Codex")
        }
        CodexProcessAction::Start if codex_launch.skipped => {
            format!("已写入本地代理启动配置；当前配置跳过自动启停，请手动启动 Codex 后使用 {target_kind} {target_name}")
        }
        CodexProcessAction::Start => {
            format!("已写入本地代理启动配置；Codex 未在运行且自动启动失败，请手动启动 Codex 后使用 {target_kind} {target_name}")
        }
        CodexProcessAction::Restart => {
            format!("已写入本地代理启动配置；请启动/重启 Codex 后使用 {target_kind} {target_name}")
        }
    }
}

#[derive(Debug, Clone)]
struct CodexLaunchTarget {
    app_name: Option<String>,
    command: Option<String>,
    process_match: Option<String>,
    skip_restart: bool,
}

impl CodexLaunchTarget {
    fn from_env() -> Self {
        let command = env_text("CODEX_COMPANION_CODEX_COMMAND");
        let app_data_dir =
            env_text("CODEX_COMPANION_CODEX_APP_DATA").or_else(|| env_text("DEV_CODEX_APP_DATA"));
        let process_match = env_text("CODEX_COMPANION_CODEX_PROCESS_MATCH")
            .or_else(|| app_data_dir.map(|path| format!("--user-data-dir={path}")));
        Self {
            app_name: env_text("CODEX_COMPANION_CODEX_APP_NAME")
                .or_else(|| command.is_none().then(|| "Codex".to_string())),
            command,
            process_match,
            skip_restart: env_flag("CODEX_COMPANION_SKIP_CODEX_RESTART"),
        }
    }
}

fn env_text(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_flag(name: &str) -> bool {
    env_text(name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(target_os = "macos")]
fn codex_running(target: &CodexLaunchTarget) -> bool {
    if let Some(pattern) = target.process_match.as_deref() {
        return process_match_running(pattern);
    }
    target
        .app_name
        .as_deref()
        .is_some_and(|app_name| process_name_running(app_name))
}

#[cfg(target_os = "windows")]
fn codex_running(target: &CodexLaunchTarget) -> bool {
    if let Some(pattern) = target.process_match.as_deref() {
        return process_match_running(pattern);
    }
    let Some(app_name) = target.app_name.as_deref() else {
        return false;
    };
    let image_name = if app_name.ends_with(".exe") {
        app_name.to_string()
    } else {
        format!("{app_name}.exe")
    };
    Command::new("tasklist")
        .args(["/FI", format!("IMAGENAME eq {image_name}").as_str()])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .to_ascii_lowercase()
                    .contains(&image_name.to_ascii_lowercase())
        })
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn codex_running(target: &CodexLaunchTarget) -> bool {
    if let Some(pattern) = target.process_match.as_deref() {
        return process_match_running(pattern);
    }
    target
        .app_name
        .as_deref()
        .is_some_and(|app_name| process_name_running(app_name))
}

#[cfg(target_os = "macos")]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    let Some(app_name) = target.app_name.as_deref() else {
        return;
    };
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(format!(r#"tell application "{app_name}" to quit"#))
        .status();
    let _ = Command::new("pkill").args(["-x", app_name]).status();
}

#[cfg(target_os = "windows")]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    let Some(app_name) = target.app_name.as_deref() else {
        return;
    };
    let image_name = if app_name.ends_with(".exe") {
        app_name.to_string()
    } else {
        format!("{app_name}.exe")
    };
    let _ = Command::new("taskkill")
        .args(["/IM", image_name.as_str(), "/F"])
        .status();
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    let Some(app_name) = target.app_name.as_deref() else {
        return;
    };
    let _ = Command::new("pkill").arg(app_name).status();
}

#[cfg(any(
    target_os = "macos",
    all(not(target_os = "windows"), not(target_os = "macos"))
))]
fn process_match_running(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", pattern])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(any(
    target_os = "macos",
    all(not(target_os = "windows"), not(target_os = "macos"))
))]
fn process_name_running(name: &str) -> bool {
    Command::new("pgrep")
        .args(["-x", name])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(any(
    target_os = "macos",
    all(not(target_os = "windows"), not(target_os = "macos"))
))]
fn kill_process_match(pattern: &str) {
    let _ = Command::new("pkill").args(["-f", pattern]).status();
}

#[cfg(target_os = "windows")]
fn process_match_running(pattern: &str) -> bool {
    let script = format!(
        "$pattern = '{}'; $found = Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains($pattern) }} | Select-Object -First 1; if ($found) {{ exit 0 }} else {{ exit 1 }}",
        powershell_single_quote(pattern)
    );
    Command::new("powershell")
        .args(["-NoProfile", "-Command", script.as_str()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "windows")]
fn kill_process_match(pattern: &str) {
    let script = format!(
        "$pattern = '{}'; Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains($pattern) }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
        powershell_single_quote(pattern)
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-Command", script.as_str()])
        .status();
}

#[cfg(target_os = "windows")]
fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "macos")]
fn start_codex(target: &CodexLaunchTarget) -> bool {
    if let Some(command) = target.command.as_deref() {
        return Command::new("/bin/sh")
            .args(["-lc", command])
            .spawn()
            .is_ok();
    }
    let app_name = target.app_name.as_deref().unwrap_or("Codex");
    Command::new("open")
        .args(["-a", app_name])
        .status()
        .is_ok_and(|status| status.success())
        || Command::new("codex").spawn().is_ok()
}

#[cfg(target_os = "windows")]
fn start_codex(target: &CodexLaunchTarget) -> bool {
    if let Some(command) = target.command.as_deref() {
        return Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/C", command])
            .status()
            .is_ok_and(|status| status.success());
    }
    let command = target.command.clone().unwrap_or_else(|| {
        target
            .app_name
            .clone()
            .unwrap_or_else(|| "codex".to_string())
    });
    Command::new("cmd")
        .args(["/C", "start", "", command.as_str()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn start_codex(target: &CodexLaunchTarget) -> bool {
    if let Some(command) = target.command.as_deref() {
        return Command::new("sh").args(["-lc", command]).spawn().is_ok();
    }
    Command::new(target.app_name.as_deref().unwrap_or("codex"))
        .spawn()
        .is_ok()
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
    fn official_provider_file_auth_can_direct_connect() {
        let provider = provider(ProviderKind::OfficialCodex, Some("file:/tmp/auth.json"));
        assert!(provider_can_direct_connect(&provider));
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

    #[test]
    fn relay_launch_requires_restart_after_direct_launch() {
        assert!(relay_restart_required(
            true,
            Some(&CodexLaunchMode::ProviderDirect),
            false
        ));
    }

    #[test]
    fn relay_launch_can_hot_switch_after_relay_launch() {
        assert!(!relay_restart_required(
            true,
            Some(&CodexLaunchMode::ProviderRelay),
            false
        ));
    }

    #[test]
    fn relay_launch_requires_restart_when_marked_pending() {
        assert!(relay_restart_required(
            true,
            Some(&CodexLaunchMode::ProviderRelay),
            true
        ));
    }

    #[test]
    fn relay_launch_starts_codex_when_not_running() {
        assert_eq!(
            codex_process_action(false, false),
            CodexProcessAction::Start
        );
    }

    #[test]
    fn relay_launch_noops_only_when_codex_is_running() {
        assert_eq!(codex_process_action(false, true), CodexProcessAction::None);
    }

    #[test]
    fn relay_launch_restart_beats_running_state() {
        assert_eq!(
            codex_process_action(true, true),
            CodexProcessAction::Restart
        );
    }

    #[test]
    fn relay_launch_message_mentions_hot_start() {
        let message = relay_launch_message(
            "provider",
            "Provider",
            false,
            CodexProcessLaunch {
                action: CodexProcessAction::Start,
                succeeded: true,
                skipped: false,
            },
        );
        assert!(message.contains("并已启动 Codex"));
    }
}
