use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    default_codex_dir, provider_endpoint_is_chat_completions, CliLaunchOutcome, CliLaunchRequest,
    CodexInstallStatus, CodexLaunchMode, CodexLaunchOutcome, CompanionError, GroupPolicy,
    ProviderConfig, ProviderGroup, ProviderKind, ProviderLaunchMode, RelayConfig, RepairOptions,
    RepairOutcome, RepairPlan, Result, TerminalKind, COMPANION_PROVIDER_ID,
};
use codex_companion_provider::{provider_uses_agent_identity, selected_providers_for_group};
use codex_companion_state::{
    doctor, install_companion_provider_with_token_source, install_direct_provider_with_options,
    repair_state, CodexInstallSnapshot, DirectInstallOptions,
};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

const SINGLE_PROVIDER_GROUP_PREFIX: &str = "single-";
pub(crate) const CODEX_OPENAI_PROVIDER_ID: &str = "openai";
const CHATGPT_APP_NAME: &str = "ChatGPT";
const LEGACY_CODEX_APP_NAME: &str = "Codex";
const CODEX_CLIENT_DISPLAY_NAME: &str = "ChatGPT / Codex";

impl CompanionDaemon {
    pub fn preview_cli_command(&self, request: &CliLaunchRequest) -> Result<String> {
        validate_working_directory(&request.working_directory)?;
        Ok(cli_shell_command(request))
    }

    pub fn launch_cli(&self, request: CliLaunchRequest) -> Result<CliLaunchOutcome> {
        validate_working_directory(&request.working_directory)?;
        let terminal = resolve_terminal(request.terminal.clone());
        let command = cli_shell_command(&request);
        let launched = launch_cli_terminal(&terminal, &request.working_directory, &command);
        self.store.update(|config| {
            config.app.preferred_terminal = terminal.clone();
            config
                .app
                .recent_working_directories
                .retain(|path| path != &request.working_directory);
            config
                .app
                .recent_working_directories
                .insert(0, request.working_directory.clone());
            config.app.recent_working_directories.truncate(8);
            Ok(())
        })?;
        Ok(CliLaunchOutcome {
            command,
            terminal,
            working_directory: request.working_directory,
            launched,
            message: if launched {
                "已在所选终端启动 Codex CLI".to_string()
            } else {
                "终端启动失败，命令已生成，可复制后手动执行".to_string()
            },
        })
    }

    pub fn launch_group(
        &self,
        group_id: &str,
        codex_dir: Option<PathBuf>,
    ) -> Result<CodexLaunchOutcome> {
        let previous_config = self.store.load()?;
        let previous_launch_mode = previous_config.app.last_codex_launch_mode.clone();
        let pending_relay_restart = previous_config.app.codex_restart_required_on_next_relay;
        let group = previous_config
            .groups
            .get(group_id)
            .cloned()
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {group_id}")))?;
        let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
        let relay_installed = doctor(codex_dir.clone(), &previous_config.relay)?.installed;
        let restart_required = relay_restart_required(
            relay_installed,
            previous_launch_mode.as_ref(),
            pending_relay_restart,
        );
        let selected = selected_providers_for_group(&previous_config, &group);
        let token_source = group_relay_token_source(&group, &selected);
        let (codex, install_snapshot) =
            install_relay_for_launch(&codex_dir, &previous_config.relay, &token_source)?;
        if let Err(error) =
            self.commit_group_relay_launch(group_id, &previous_config.relay.base_url())
        {
            return Err(rollback_launch_install(install_snapshot, error));
        }
        let repair = repair_for_launch(&codex_dir, COMPANION_PROVIDER_ID.to_string());
        let codex_launch = ensure_codex_started(restart_required);

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::GroupRelay,
            target_id: group.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            message: with_codex_status(
                relay_launch_message("分组", &group.name, restart_required, codex_launch),
                &codex.message,
            ),
            repair,
            restart_required,
            codex_started: codex_launch.codex_started(),
            codex,
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
                    if provider_endpoint_is_chat_completions(&provider.base_url) {
                        return Err(CompanionError::InvalidConfig(format!(
                            "provider {} 只提供 Chat Completions 接口；ChatGPT / Codex 直连发送 Responses 请求，必须改用本地代理完成协议转换",
                            provider.name
                        )));
                    }
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
            let install_snapshot = CodexInstallSnapshot::capture(&codex_dir)?;
            let codex = install_direct_provider_with_options(
                Some(codex_dir.clone()),
                &provider,
                DirectInstallOptions {
                    preserve_official_codex_auth: config_snapshot.app.preserve_official_codex_auth,
                },
            )?;
            if let Err(error) = self.commit_direct_provider_launch(
                &provider,
                config_snapshot.app.preserve_official_codex_auth,
            ) {
                return Err(rollback_launch_install(install_snapshot, error));
            }
            let target_provider_id = direct_repair_target_provider_id(&provider);
            let repair = repair_for_launch(&codex_dir, target_provider_id.clone());
            let restart_required = true;
            let codex_launch = restart_codex();
            return Ok(CodexLaunchOutcome {
                mode: CodexLaunchMode::ProviderDirect,
                target_id: provider.id.clone(),
                target_provider_id,
                message: with_codex_status(
                    direct_launch_message(&provider.name, codex_launch),
                    &codex.message,
                ),
                repair,
                restart_required,
                codex_started: codex_launch.codex_started(),
                codex,
            });
        }

        let relay_installed = doctor(codex_dir.clone(), &config_snapshot.relay)?.installed;
        let restart_required = relay_restart_required(
            relay_installed,
            previous_launch_mode.as_ref(),
            pending_relay_restart,
        );
        let token_source = provider_relay_token_source(&provider);
        let (codex, install_snapshot) =
            install_relay_for_launch(&codex_dir, &config_snapshot.relay, &token_source)?;
        if let Err(error) =
            self.commit_provider_relay_launch(&provider.id, &config_snapshot.relay.base_url())
        {
            return Err(rollback_launch_install(install_snapshot, error));
        }
        let repair = repair_for_launch(&codex_dir, COMPANION_PROVIDER_ID.to_string());
        let codex_launch = ensure_codex_started(restart_required);

        Ok(CodexLaunchOutcome {
            mode: CodexLaunchMode::ProviderRelay,
            target_id: provider.id.clone(),
            target_provider_id: COMPANION_PROVIDER_ID.to_string(),
            message: with_codex_status(
                format!(
                    "{}，原因：{}",
                    relay_launch_message(
                        "provider",
                        &provider.name,
                        restart_required,
                        codex_launch
                    ),
                    provider_relay_reason(&provider)
                ),
                &codex.message,
            ),
            repair,
            restart_required,
            codex_started: codex_launch.codex_started(),
            codex,
        })
    }

    fn commit_direct_provider_launch(
        &self,
        expected_provider: &ProviderConfig,
        expected_preserve_official_auth: bool,
    ) -> Result<()> {
        self.store.update(|config| {
            let current = config
                .providers
                .get(&expected_provider.id)
                .ok_or_else(|| {
                    CompanionError::InvalidConfig(format!(
                        "unknown provider: {}",
                        expected_provider.id
                    ))
                })?;
            if !same_direct_install_target(current, expected_provider)
                || config.app.preserve_official_codex_auth != expected_preserve_official_auth
            {
                return Err(CompanionError::InvalidConfig(format!(
                    "provider {} 的直连配置在启动过程中发生变化，已取消本次切换以避免 Codex 使用旧配置",
                    expected_provider.id
                )));
            }
            config.app.last_codex_launch_mode = Some(CodexLaunchMode::ProviderDirect);
            config.app.last_codex_target_provider_id = Some(expected_provider.id.clone());
            config.app.codex_restart_required_on_next_relay = false;
            Ok(())
        })
    }

    fn commit_group_relay_launch(
        &self,
        group_id: &str,
        expected_relay_base_url: &str,
    ) -> Result<()> {
        self.store.update(|config| {
            ensure_relay_install_is_current(config, expected_relay_base_url)?;
            if !config.groups.contains_key(group_id) {
                return Err(CompanionError::InvalidConfig(format!(
                    "unknown group: {group_id}"
                )));
            }
            config.relay.active_group_id = group_id.to_string();
            record_relay_launch(config, CodexLaunchMode::GroupRelay);
            Ok(())
        })
    }

    fn commit_provider_relay_launch(
        &self,
        provider_id: &str,
        expected_relay_base_url: &str,
    ) -> Result<ProviderGroup> {
        self.store.update(|config| {
            ensure_relay_install_is_current(config, expected_relay_base_url)?;
            let provider = config.providers.get(provider_id).cloned().ok_or_else(|| {
                CompanionError::InvalidConfig(format!("unknown provider: {provider_id}"))
            })?;
            let group = single_provider_group(&provider);
            config.groups.insert(group.id.clone(), group.clone());
            config.relay.active_group_id = group.id.clone();
            record_relay_launch(config, CodexLaunchMode::ProviderRelay);
            Ok(group)
        })
    }
}

fn same_direct_install_target(current: &ProviderConfig, expected: &ProviderConfig) -> bool {
    current.id == expected.id
        && current.name == expected.name
        && current.kind == expected.kind
        && current.base_url == expected.base_url
        && effective_direct_auth_ref(current) == effective_direct_auth_ref(expected)
}

fn effective_direct_auth_ref(provider: &ProviderConfig) -> Option<&str> {
    provider
        .direct_auth_ref
        .as_deref()
        .or(provider.auth_ref.as_deref())
}

fn ensure_relay_install_is_current(
    config: &codex_companion_core::CompanionConfig,
    expected_relay_base_url: &str,
) -> Result<()> {
    let current = config.relay.base_url();
    if current == expected_relay_base_url {
        return Ok(());
    }
    Err(CompanionError::InvalidConfig(format!(
        "本地代理地址在启动过程中从 {expected_relay_base_url} 变更为 {current}，已取消本次切换以避免 Codex 连接旧地址"
    )))
}

fn record_relay_launch(config: &mut codex_companion_core::CompanionConfig, mode: CodexLaunchMode) {
    config.app.last_codex_launch_mode = Some(mode);
    config.app.last_codex_target_provider_id = Some(COMPANION_PROVIDER_ID.to_string());
    config.app.codex_restart_required_on_next_relay = false;
}

fn install_relay_for_launch(
    codex_dir: &Path,
    relay: &RelayConfig,
    token_source: &str,
) -> Result<(CodexInstallStatus, CodexInstallSnapshot)> {
    let snapshot = CodexInstallSnapshot::capture(codex_dir)?;
    let codex = install_companion_provider_with_token_source(
        Some(codex_dir.to_path_buf()),
        relay,
        Some(token_source),
    )?;
    Ok((codex, snapshot))
}

fn rollback_launch_install(
    snapshot: CodexInstallSnapshot,
    error: CompanionError,
) -> CompanionError {
    match snapshot.restore() {
        Ok(()) => {
            CompanionError::InvalidConfig(format!("{error}；启动未完成，Codex 配置已自动恢复"))
        }
        Err(rollback_error) => CompanionError::InvalidConfig(format!(
            "{error}；自动恢复 Codex 配置失败: {rollback_error}"
        )),
    }
}

fn validate_working_directory(path: &std::path::Path) -> Result<()> {
    if !path.is_dir() {
        return Err(CompanionError::InvalidConfig(format!(
            "CLI 工作目录不存在或不是目录: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn cli_shell_command(request: &CliLaunchRequest) -> String {
    let codex_command = request
        .resume_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|session_id| format!("codex resume {}", shell_quote(session_id)))
        .unwrap_or_else(|| "codex".to_string());
    format!(
        "cd {} && {codex_command}",
        shell_quote(&request.working_directory.to_string_lossy())
    )
}

#[cfg(target_os = "windows")]
fn cli_shell_command(request: &CliLaunchRequest) -> String {
    let terminal = resolve_terminal(request.terminal.clone());
    let resume_session_id = request
        .resume_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if matches!(terminal, TerminalKind::PowerShell | TerminalKind::Pwsh) {
        let working_directory =
            powershell_single_quote(&request.working_directory.to_string_lossy());
        let codex_command = resume_session_id
            .map(|session_id| format!("codex resume '{}'", powershell_single_quote(session_id)))
            .unwrap_or_else(|| "codex".to_string());
        return format!("Set-Location -LiteralPath '{working_directory}'; {codex_command}");
    }
    let working_directory = cmd_quote(&request.working_directory.to_string_lossy());
    let codex_command = resume_session_id
        .map(|session_id| format!("codex resume {}", cmd_quote(session_id)))
        .unwrap_or_else(|| "codex".to_string());
    format!("cd /d {working_directory} && {codex_command}")
}

#[cfg(not(target_os = "windows"))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn cmd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn resolve_terminal(terminal: TerminalKind) -> TerminalKind {
    if !matches!(terminal, TerminalKind::Auto) {
        return terminal;
    }
    #[cfg(target_os = "macos")]
    return TerminalKind::Terminal;
    #[cfg(target_os = "windows")]
    return TerminalKind::WindowsTerminal;
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    TerminalKind::Shell
}

#[cfg(target_os = "macos")]
fn launch_cli_terminal(
    terminal: &TerminalKind,
    _working_directory: &std::path::Path,
    command: &str,
) -> bool {
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    let script = match terminal {
        TerminalKind::ITerm2 => format!(
            r#"tell application "iTerm2"
activate
create window with default profile
tell current session of current window to write text "{escaped}"
end tell"#
        ),
        _ => format!(
            r#"tell application "Terminal"
activate
do script "{escaped}"
end tell"#
        ),
    };
    Command::new("osascript")
        .args(["-e", script.as_str()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "windows")]
fn launch_cli_terminal(
    terminal: &TerminalKind,
    working_directory: &std::path::Path,
    command: &str,
) -> bool {
    let working_directory = working_directory.to_string_lossy();
    match terminal {
        TerminalKind::PowerShell => Command::new("powershell")
            .args(["-NoExit", "-Command", command])
            .current_dir(working_directory.as_ref())
            .spawn()
            .is_ok(),
        TerminalKind::Pwsh => Command::new("pwsh")
            .args(["-NoExit", "-Command", command])
            .current_dir(working_directory.as_ref())
            .spawn()
            .is_ok(),
        TerminalKind::Cmd => Command::new("cmd")
            .args(["/K", command])
            .current_dir(working_directory.as_ref())
            .spawn()
            .is_ok(),
        _ => Command::new("wt")
            .args(["-d", working_directory.as_ref(), "cmd", "/K", command])
            .spawn()
            .is_ok(),
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn launch_cli_terminal(
    _terminal: &TerminalKind,
    working_directory: &std::path::Path,
    command: &str,
) -> bool {
    for (program, args) in [
        ("x-terminal-emulator", vec!["-e", "sh", "-lc", command]),
        ("gnome-terminal", vec!["--", "sh", "-lc", command]),
        ("konsole", vec!["-e", "sh", "-lc", command]),
    ] {
        if Command::new(program)
            .args(args)
            .current_dir(working_directory)
            .spawn()
            .is_ok()
        {
            return true;
        }
    }
    false
}

pub fn provider_can_direct_connect(provider: &ProviderConfig) -> bool {
    !provider_uses_agent_identity(provider)
        && !provider_endpoint_is_chat_completions(&provider.base_url)
        && provider
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
        if provider_uses_agent_identity(provider) {
            return "Agent Identity 仅通过 Companion API 服务动态签名，不写入 Codex auth.json";
        }
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

fn provider_relay_token_source(provider: &ProviderConfig) -> String {
    let source = provider
        .auth_ref
        .as_deref()
        .or(provider.direct_auth_ref.as_deref())
        .map(str::trim)
        .filter(|auth_ref| !auth_ref.is_empty());
    if matches!(provider.kind, ProviderKind::OfficialCodex) {
        return format!(
            "Companion relay injection from official Codex provider {}",
            provider.name
        );
    }
    if let Some(env_var) = source.and_then(|auth_ref| auth_ref.strip_prefix("env:")) {
        return format!(
            "Companion relay injection from environment variable {} for provider {}",
            env_var.trim(),
            provider.name
        );
    }
    if source.is_some_and(|auth_ref| auth_ref.starts_with("file:")) {
        return format!(
            "Companion relay injection from Companion auth file for provider {}",
            provider.name
        );
    }
    format!(
        "Companion relay injection from selected provider {}",
        provider.name
    )
}

fn group_relay_token_source(group: &ProviderGroup, providers: &[ProviderConfig]) -> String {
    match providers {
        [] => format!(
            "Companion relay injection from active group {} with no enabled providers",
            group.name
        ),
        [provider] => provider_relay_token_source(provider),
        providers => {
            let names = providers
                .iter()
                .map(|provider| provider.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Companion relay injection from active group {} providers: {}",
                group.name, names
            )
        }
    }
}

pub fn single_provider_group_id(provider: &ProviderConfig) -> String {
    format!("{SINGLE_PROVIDER_GROUP_PREFIX}{}", provider.id)
}

fn single_provider_group(provider: &ProviderConfig) -> ProviderGroup {
    ProviderGroup {
        id: single_provider_group_id(provider),
        name: format!("{} 单 Provider", provider.name),
        policy: GroupPolicy::Manual,
        provider_order: vec![provider.id.clone()],
        provider_weights: Default::default(),
        fallback_enabled: false,
    }
}

pub(crate) fn direct_repair_target_provider_id(provider: &ProviderConfig) -> String {
    if matches!(provider.kind, ProviderKind::OfficialCodex) {
        CODEX_OPENAI_PROVIDER_ID.to_string()
    } else {
        provider.id.clone()
    }
}

fn repair_for_launch(codex_dir: &std::path::Path, target_provider_id: String) -> RepairOutcome {
    repair_state(RepairOptions {
        codex_dir: codex_dir.to_path_buf(),
        history: true,
        plugins: true,
        dry_run: false,
        target_provider_id: Some(target_provider_id.clone()),
    })
    .unwrap_or_else(|error| {
        skipped_launch_repair(
            codex_dir,
            target_provider_id,
            format!("启动前修复未完成，已跳过且不阻塞 Codex 启动: {error}"),
        )
    })
}

fn skipped_launch_repair(
    codex_dir: &std::path::Path,
    target_provider_id: String,
    reason: String,
) -> RepairOutcome {
    RepairOutcome {
        plan: RepairPlan {
            codex_dir: codex_dir.to_path_buf(),
            target_provider_id,
            history_files: 0,
            history_lines: 0,
            plugin_files: 0,
            state_rows: 0,
            source_provider_ids: Vec::new(),
            dry_run: false,
        },
        backup_root: None,
        migrated_history_files: 0,
        migrated_history_lines: 0,
        migrated_plugin_files: 0,
        migrated_state_rows: 0,
        skipped_reason: Some(reason),
    }
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
        return format!(
            "已直连启动 provider {provider_name}，并已重启 {CODEX_CLIENT_DISPLAY_NAME} 以载入账号/API Key"
        );
    }
    if codex_launch.skipped {
        return format!(
            "已写入直连 provider {provider_name}；当前配置跳过自动启停，请手动启动/重启 {CODEX_CLIENT_DISPLAY_NAME}"
        );
    }
    format!(
        "已写入直连 provider {provider_name}；直连模式需要启动/重启 {CODEX_CLIENT_DISPLAY_NAME} 后才会生效"
    )
}

fn with_codex_status(message: String, codex_status: &str) -> String {
    if codex_status.trim().is_empty() || message.contains(codex_status) {
        message
    } else {
        format!("{message}；{codex_status}")
    }
}

fn relay_launch_message(
    target_kind: &str,
    target_name: &str,
    restart_required: bool,
    codex_launch: CodexProcessLaunch,
) -> String {
    if restart_required {
        if codex_launch.codex_started() {
            return format!(
                "已通过本地代理启动 {target_kind} {target_name}，并已重启 {CODEX_CLIENT_DISPLAY_NAME}"
            );
        }
        if codex_launch.skipped {
            return format!(
                "已写入本地代理启动配置；当前配置跳过自动启停，请手动启动/重启 {CODEX_CLIENT_DISPLAY_NAME} 后使用 {target_kind} {target_name}"
            );
        }
        return format!(
            "已写入本地代理启动配置；请启动/重启 {CODEX_CLIENT_DISPLAY_NAME} 后使用 {target_kind} {target_name}"
        );
    }

    match codex_launch.action {
        CodexProcessAction::None => {
            format!("已切换本地代理到 {target_kind} {target_name}，Codex 已在本地代理模式运行，无需重启")
        }
        CodexProcessAction::Start if codex_launch.succeeded => {
            format!(
                "已切换本地代理到 {target_kind} {target_name}，并已启动 {CODEX_CLIENT_DISPLAY_NAME}"
            )
        }
        CodexProcessAction::Start if codex_launch.skipped => {
            format!("已写入本地代理启动配置；当前配置跳过自动启停，请手动启动 {CODEX_CLIENT_DISPLAY_NAME} 后使用 {target_kind} {target_name}")
        }
        CodexProcessAction::Start => {
            format!("已写入本地代理启动配置；{CODEX_CLIENT_DISPLAY_NAME} 未在运行且自动启动失败，请手动启动后使用 {target_kind} {target_name}")
        }
        CodexProcessAction::Restart => {
            format!("已写入本地代理启动配置；请启动/重启 {CODEX_CLIENT_DISPLAY_NAME} 后使用 {target_kind} {target_name}")
        }
    }
}

#[derive(Debug, Clone)]
struct CodexLaunchTarget {
    app_names: Vec<String>,
    command: Option<String>,
    process_match: Option<String>,
    skip_restart: bool,
}

impl CodexLaunchTarget {
    fn from_env() -> Self {
        let command = env_text("CODEX_COMPANION_CLIENT_COMMAND")
            .or_else(|| env_text("CODEX_COMPANION_CODEX_COMMAND"));
        let app_data_dir = env_text("CODEX_COMPANION_CLIENT_APP_DATA")
            .or_else(|| env_text("CODEX_COMPANION_CODEX_APP_DATA"))
            .or_else(|| env_text("DEV_CODEX_APP_DATA"));
        let process_match = env_text("CODEX_COMPANION_CLIENT_PROCESS_MATCH")
            .or_else(|| env_text("CODEX_COMPANION_CODEX_PROCESS_MATCH"))
            .or_else(|| app_data_dir.map(|path| format!("--user-data-dir={path}")));
        let explicit_app_name = env_text("CODEX_COMPANION_CLIENT_APP_NAME")
            .or_else(|| env_text("CODEX_COMPANION_CODEX_APP_NAME"));
        Self {
            app_names: explicit_app_name.map(|name| vec![name]).unwrap_or_else(|| {
                command
                    .is_none()
                    .then(default_codex_app_names)
                    .unwrap_or_default()
            }),
            command,
            process_match,
            skip_restart: env_flag("CODEX_COMPANION_SKIP_CLIENT_RESTART")
                || env_flag("CODEX_COMPANION_SKIP_CODEX_RESTART"),
        }
    }
}

fn default_codex_app_names() -> Vec<String> {
    vec![
        CHATGPT_APP_NAME.to_string(),
        LEGACY_CODEX_APP_NAME.to_string(),
    ]
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
        .app_names
        .iter()
        .any(|app_name| process_name_running(app_name))
}

#[cfg(target_os = "windows")]
fn codex_running(target: &CodexLaunchTarget) -> bool {
    if let Some(pattern) = target.process_match.as_deref() {
        return process_match_running(pattern);
    }
    Command::new("tasklist").output().is_ok_and(|output| {
        if !output.status.success() {
            return false;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        target.app_names.iter().any(|app_name| {
            let image_name = windows_image_name(app_name);
            stdout.contains(&image_name.to_ascii_lowercase())
        })
    })
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn codex_running(target: &CodexLaunchTarget) -> bool {
    if let Some(pattern) = target.process_match.as_deref() {
        return process_match_running(pattern);
    }
    target
        .app_names
        .iter()
        .any(|app_name| process_name_running(app_name))
}

#[cfg(target_os = "macos")]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    for app_name in &target.app_names {
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!(r#"tell application "{app_name}" to quit"#))
            .status();
        let _ = Command::new("pkill").args(["-x", app_name]).status();
    }
}

#[cfg(target_os = "windows")]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    for app_name in &target.app_names {
        let image_name = windows_image_name(app_name);
        let _ = Command::new("taskkill")
            .args(["/IM", image_name.as_str(), "/F"])
            .status();
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn stop_codex(target: &CodexLaunchTarget) {
    if let Some(pattern) = target.process_match.as_deref() {
        kill_process_match(pattern);
        return;
    }
    for app_name in &target.app_names {
        let _ = Command::new("pkill").arg(app_name).status();
    }
}

#[cfg(target_os = "windows")]
fn windows_image_name(app_name: &str) -> String {
    if app_name.to_ascii_lowercase().ends_with(".exe") {
        app_name.to_string()
    } else {
        format!("{app_name}.exe")
    }
}

#[cfg(any(
    target_os = "macos",
    all(not(target_os = "windows"), not(target_os = "macos"))
))]
fn process_match_running(pattern: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", "--", pattern])
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
    let _ = Command::new("pkill").args(["-f", "--", pattern]).status();
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
    target.app_names.iter().any(|app_name| {
        Command::new("open")
            .args(["-a", app_name])
            .status()
            .is_ok_and(|status| status.success())
    }) || Command::new("codex").spawn().is_ok()
}

#[cfg(target_os = "windows")]
fn start_codex(target: &CodexLaunchTarget) -> bool {
    if let Some(command) = target.command.as_deref() {
        return Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/C", command])
            .status()
            .is_ok_and(|status| status.success());
    }
    if start_windows_store_codex_app() {
        return true;
    }
    target.app_names.iter().any(|app_name| {
        Command::new("cmd")
            .args(["/C", "start", "", app_name])
            .status()
            .is_ok_and(|status| status.success())
    }) || Command::new("codex").spawn().is_ok()
}

#[cfg(target_os = "windows")]
fn start_windows_store_codex_app() -> bool {
    const SCRIPT: &str = r#"$entry = Get-StartApps |
  Where-Object {
    $_.AppID -like 'OpenAI.ChatGPT*' -or
    $_.AppID -like 'OpenAI.Codex_*' -or
    $_.Name -like 'ChatGPT*' -or
    $_.Name -like 'Codex*'
  } |
  Sort-Object @{ Expression = { if ($_.AppID -like 'OpenAI.ChatGPT*' -or $_.Name -like 'ChatGPT*') { 0 } else { 1 } } }, Name |
  Select-Object -First 1
if (-not $entry -or [string]::IsNullOrWhiteSpace($entry.AppID)) { exit 1 }
Start-Process explorer.exe -ArgumentList ('shell:AppsFolder\' + $entry.AppID)
exit 0"#;
    Command::new("powershell")
        .args(["-NoProfile", "-Command", SCRIPT])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn start_codex(target: &CodexLaunchTarget) -> bool {
    if let Some(command) = target.command.as_deref() {
        return Command::new("sh").args(["-lc", command]).spawn().is_ok();
    }
    target
        .app_names
        .iter()
        .any(|app_name| Command::new(app_name).spawn().is_ok())
        || Command::new("codex").spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{
        default_refresh_interval_seconds, ConfigStore, HealthStatusKind, ProviderHealth,
        DEFAULT_GROUP_ID,
    };
    use std::collections::BTreeMap;
    use std::fs;

    fn provider(kind: ProviderKind, auth_ref: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: "p1".to_string(),
            name: "Provider".to_string(),
            kind,
            base_url: "https://example.com/v1".to_string(),
            websocket_url: None,
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
    fn chat_completions_only_provider_requires_relay() {
        let mut provider = provider(
            ProviderKind::OpenAiCompatible,
            Some("env:OPENROUTER_API_KEY"),
        );
        provider.base_url = "https://example.com/v1/chat/completions".to_string();

        assert!(!provider_can_direct_connect(&provider));
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
        assert!(message.contains("并已启动 ChatGPT / Codex"));
    }

    #[test]
    fn default_client_names_prefer_chatgpt_and_keep_legacy_codex() {
        assert_eq!(
            default_codex_app_names(),
            vec![
                CHATGPT_APP_NAME.to_string(),
                LEGACY_CODEX_APP_NAME.to_string()
            ]
        );
    }

    #[test]
    fn launch_message_includes_codex_token_source_status() {
        let message = with_codex_status(
            "已写入直连 provider Provider".to_string(),
            "Codex 已直连 provider: Provider；Token source: API key file copied into Codex auth.json；warning: direct API key mode",
        );
        assert!(message.contains("Token source: API key file"));
        assert!(message.contains("warning: direct API key mode"));
    }

    #[test]
    fn relay_token_source_uses_selected_provider_auth_ref() {
        let provider = provider(
            ProviderKind::OpenAiCompatible,
            Some("env:OPENROUTER_API_KEY"),
        );
        let source = provider_relay_token_source(&provider);

        assert!(source.contains("environment variable OPENROUTER_API_KEY"));
        assert!(source.contains("Provider"));
    }

    #[test]
    fn failed_group_launch_preserves_active_group_and_launch_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.groups.insert(
                    "other".to_string(),
                    ProviderGroup {
                        id: "other".to_string(),
                        name: "Other".to_string(),
                        policy: GroupPolicy::Manual,
                        provider_order: Vec::new(),
                        provider_weights: Default::default(),
                        fallback_enabled: false,
                    },
                );
                Ok(())
            })
            .expect("seed config");
        let invalid_codex_dir = temp.path().join("not-a-directory");
        fs::write(&invalid_codex_dir, b"file").expect("invalid codex dir");
        let daemon = CompanionDaemon::new(store.clone());

        assert!(daemon
            .launch_group("other", Some(invalid_codex_dir))
            .is_err());

        let config = store.load().expect("load config");
        assert_eq!(config.relay.active_group_id, DEFAULT_GROUP_ID);
        assert_eq!(config.app.last_codex_launch_mode, None);
        assert_eq!(config.app.last_codex_target_provider_id, None);
    }

    #[test]
    fn failed_provider_relay_launch_preserves_group_and_health() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = provider(ProviderKind::OpenAiCompatible, None);
        store
            .update(|config| {
                config
                    .providers
                    .insert(provider.id.clone(), provider.clone());
                config.health.insert(
                    provider.id.clone(),
                    ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        ..ProviderHealth::default()
                    },
                );
                Ok(())
            })
            .expect("seed config");
        let invalid_codex_dir = temp.path().join("not-a-directory");
        fs::write(&invalid_codex_dir, b"file").expect("invalid codex dir");
        let daemon = CompanionDaemon::new(store.clone());

        assert!(daemon
            .launch_provider_with_mode(
                &provider.id,
                Some(invalid_codex_dir),
                ProviderLaunchMode::Relay,
            )
            .is_err());

        let config = store.load().expect("load config");
        assert_eq!(config.relay.active_group_id, DEFAULT_GROUP_ID);
        assert!(!config
            .groups
            .contains_key(&single_provider_group_id(&provider)));
        assert_eq!(
            config.health.get(&provider.id).map(|health| &health.status),
            Some(&HealthStatusKind::AuthFailed)
        );
        assert_eq!(config.app.last_codex_launch_mode, None);
    }

    #[test]
    fn provider_relay_commit_rejects_a_provider_removed_after_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = provider(ProviderKind::OpenAiCompatible, None);
        store
            .update(|config| {
                config
                    .providers
                    .insert(provider.id.clone(), provider.clone());
                Ok(())
            })
            .expect("seed provider");
        let expected_relay_base_url = store.load().expect("config").relay.base_url();
        let daemon = CompanionDaemon::new(store.clone());
        store
            .update(|config| {
                config.providers.remove(&provider.id);
                Ok(())
            })
            .expect("remove provider");

        assert!(daemon
            .commit_provider_relay_launch(&provider.id, &expected_relay_base_url)
            .is_err());

        let config = store.load().expect("load config");
        assert_eq!(config.relay.active_group_id, DEFAULT_GROUP_ID);
        assert!(!config
            .groups
            .contains_key(&single_provider_group_id(&provider)));
        assert_eq!(config.app.last_codex_launch_mode, None);
    }

    #[test]
    fn provider_relay_commit_rejects_a_changed_relay_address() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = provider(ProviderKind::OpenAiCompatible, None);
        store
            .update(|config| {
                config
                    .providers
                    .insert(provider.id.clone(), provider.clone());
                Ok(())
            })
            .expect("seed provider");
        let expected_relay_base_url = store.load().expect("config").relay.base_url();
        store
            .update(|config| {
                config.relay.port = config.relay.port.saturating_add(1);
                Ok(())
            })
            .expect("change relay address");
        let daemon = CompanionDaemon::new(store.clone());

        assert!(daemon
            .commit_provider_relay_launch(&provider.id, &expected_relay_base_url)
            .is_err());

        let config = store.load().expect("load config");
        assert!(!config
            .groups
            .contains_key(&single_provider_group_id(&provider)));
        assert_eq!(config.relay.active_group_id, DEFAULT_GROUP_ID);
        assert_eq!(config.app.last_codex_launch_mode, None);
    }

    #[test]
    fn direct_provider_commit_rejects_a_changed_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let provider = provider(
            ProviderKind::OpenAiCompatible,
            Some("env:CODEX_COMPANION_TEST_KEY"),
        );
        store
            .update(|config| {
                config
                    .providers
                    .insert(provider.id.clone(), provider.clone());
                Ok(())
            })
            .expect("seed provider");
        store
            .update(|config| {
                config
                    .providers
                    .get_mut(&provider.id)
                    .expect("provider")
                    .base_url = "https://changed.example.com/v1".to_string();
                Ok(())
            })
            .expect("change provider");
        let daemon = CompanionDaemon::new(store.clone());

        assert!(daemon
            .commit_direct_provider_launch(&provider, false)
            .is_err());

        let config = store.load().expect("load config");
        assert_eq!(config.app.last_codex_launch_mode, None);
        assert_eq!(config.app.last_codex_target_provider_id, None);
    }

    #[cfg(unix)]
    #[test]
    fn companion_commit_failure_restores_codex_install_state() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempdir().expect("tempdir");
        let companion_config = temp.path().join("companion-config.json");
        fs::write(
            &companion_config,
            serde_json::to_string_pretty(&codex_companion_core::CompanionConfig::default())
                .expect("serialize config"),
        )
        .expect("write companion config");
        let companion_file = fs::File::open(&companion_config).expect("open companion config");
        let store = ConfigStore::new(PathBuf::from(format!(
            "/dev/fd/{}",
            companion_file.as_raw_fd()
        )));

        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        let original_config = concat!(
            "model_provider = \"openai\"\n\n",
            "[model_providers.openai]\n",
            "name = \"OpenAI\"\n",
            "base_url = \"https://api.openai.com/v1\"\n"
        );
        fs::write(codex_dir.join("config.toml"), original_config).expect("codex config");

        let daemon = CompanionDaemon::new(store);
        let error = daemon
            .launch_group(DEFAULT_GROUP_ID, Some(codex_dir.clone()))
            .expect_err("companion commit must fail");

        assert!(
            error.to_string().contains("Codex 配置已自动恢复"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(codex_dir.join("config.toml")).expect("restored config"),
            original_config
        );
        assert!(!codex_dir
            .join("backups/codex-companion/managed-state.json")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn direct_launch_commit_failure_restores_codex_install_state() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempdir().expect("tempdir");
        let provider = provider(
            ProviderKind::OpenAiCompatible,
            Some("env:CODEX_COMPANION_TEST_KEY"),
        );
        let mut companion = codex_companion_core::CompanionConfig::default();
        companion
            .providers
            .insert(provider.id.clone(), provider.clone());
        let companion_config = temp.path().join("companion-config.json");
        fs::write(
            &companion_config,
            serde_json::to_string_pretty(&companion).expect("serialize config"),
        )
        .expect("write companion config");
        let companion_file = fs::File::open(&companion_config).expect("open companion config");
        let store = ConfigStore::new(PathBuf::from(format!(
            "/dev/fd/{}",
            companion_file.as_raw_fd()
        )));

        let codex_dir = temp.path().join("codex");
        fs::create_dir_all(&codex_dir).expect("codex dir");
        let original_config = "model_provider = \"openai\"\n";
        fs::write(codex_dir.join("config.toml"), original_config).expect("codex config");

        let daemon = CompanionDaemon::new(store);
        let error = daemon
            .launch_provider_with_mode(
                &provider.id,
                Some(codex_dir.clone()),
                ProviderLaunchMode::Direct,
            )
            .expect_err("companion commit must fail");

        assert!(
            error.to_string().contains("Codex 配置已自动恢复"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(codex_dir.join("config.toml")).expect("restored config"),
            original_config
        );
        assert!(!codex_dir
            .join("backups/codex-companion/managed-state.json")
            .exists());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cli_command_quotes_working_directory_and_resume_session_id() {
        let request = CliLaunchRequest {
            working_directory: PathBuf::from("/tmp/Client's project"),
            terminal: TerminalKind::Shell,
            resume_session_id: Some("session'; touch /tmp/unexpected #".to_string()),
        };

        assert_eq!(
            cli_shell_command(&request),
            "cd '/tmp/Client'\\''s project' && codex resume 'session'\\''; touch /tmp/unexpected #'"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cli_command_ignores_an_empty_resume_session_id() {
        let request = CliLaunchRequest {
            working_directory: PathBuf::from("/tmp/project with spaces"),
            terminal: TerminalKind::Shell,
            resume_session_id: Some("   ".to_string()),
        };

        assert_eq!(
            cli_shell_command(&request),
            "cd '/tmp/project with spaces' && codex"
        );
    }
}
