use codex_companion_core::{
    default_codex_dir, ApiClientCreate, ApiClientUpdate, HealthStatusKind, ProviderLaunchMode,
    ProviderViewMode, RelaySettingsUpdate, RepairOptions, ThemeMode,
};
use codex_companion_daemon::CompanionDaemon;
use codex_companion_provider::{
    ApiKeyProviderImportRequest, ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat,
    ProviderUpsert,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ICON_ID: &str = "main-tray";
const TRAY_ACTION_EVENT: &str = "tray-action";
const TRAY_STATUS_ID: &str = "tray-status";
const TRAY_ROUTE_ID: &str = "tray-route";
const TRAY_OPEN_ID: &str = "tray-open";
const TRAY_HIDE_ID: &str = "tray-hide";
const TRAY_QUIT_ID: &str = "tray-quit";
const TRAY_LAUNCH_ID: &str = "tray-launch";
const TRAY_REFRESH_ID: &str = "tray-refresh";
const TRAY_LOGS_ID: &str = "tray-logs";
const TRAY_DASHBOARD_ID: &str = "tray-dashboard";
const TRAY_PROVIDERS_ID: &str = "tray-providers";
const TRAY_GROUPS_ID: &str = "tray-groups";
const TRAY_RELAY_ID: &str = "tray-relay";
const TRAY_TOKEN_ID: &str = "tray-token";
const TRAY_SESSIONS_ID: &str = "tray-sessions";
const TRAY_REPAIR_ID: &str = "tray-repair";
const TRAY_SETTINGS_ID: &str = "tray-settings";

struct TrayMenuLabels {
    runtime: String,
    route: String,
    launch: String,
    can_launch: bool,
}

fn tray_menu_labels() -> TrayMenuLabels {
    let Ok(status) = daemon().and_then(|daemon| daemon.status().map_err(|error| error.to_string()))
    else {
        return TrayMenuLabels {
            runtime: "● Companion 正在启动".to_string(),
            route: "当前分组：读取中".to_string(),
            launch: "启动 Codex（读取配置中）".to_string(),
            can_launch: false,
        };
    };

    let total = status.active_providers.len();
    let available = status
        .active_providers
        .iter()
        .filter(|provider| {
            matches!(
                status
                    .config
                    .health
                    .get(&provider.id)
                    .map(|health| &health.status),
                None | Some(HealthStatusKind::Healthy | HealthStatusKind::Unknown)
            )
        })
        .count();
    let Some(group) = status.active_group else {
        return TrayMenuLabels {
            runtime: format!("● Companion 正在运行 · {}", status.relay_base_url),
            route: "当前分组：未配置".to_string(),
            launch: "启动 Codex（需先配置分组）".to_string(),
            can_launch: false,
        };
    };
    let can_launch = total > 0;
    let launch = if can_launch {
        format!("启动 Codex（{}）", group.name)
    } else {
        "启动 Codex（分组暂无账号）".to_string()
    };

    TrayMenuLabels {
        runtime: format!("● Companion 正在运行 · {}", status.relay_base_url),
        route: format!("当前分组：{} · 可用账号 {available}/{total}", group.name),
        launch,
        can_launch,
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn hide_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.hide();
    }
}

fn toggle_main_window<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return;
    };

    let is_visible = window.is_visible().unwrap_or(false);
    let is_minimized = window.is_minimized().unwrap_or(false);
    if is_visible && !is_minimized {
        let _ = window.hide();
    } else {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn emit_tray_action<R: Runtime>(app: &AppHandle<R>, action: &str, reveal_window: bool) {
    if reveal_window {
        show_main_window(app);
    }
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        if let Err(error) = window.emit(TRAY_ACTION_EVENT, action) {
            eprintln!("Codex Companion tray action failed: {error}");
        }
    }
}

fn open_tray_diagnostics() {
    if let Err(error) = daemon().and_then(|daemon| {
        daemon
            .open_diagnostic_directory()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        eprintln!("Codex Companion could not open diagnostics from tray: {error}");
    }
}

fn daemon() -> Result<CompanionDaemon, String> {
    CompanionDaemon::default().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_status() -> Result<codex_companion_core::CompanionStatus, String> {
    daemon()?.status().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_api_service_snapshot() -> Result<codex_companion_core::ApiServiceSnapshot, String> {
    daemon()?
        .api_service_snapshot()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_api_request_logs() -> Result<Vec<codex_companion_core::ApiRequestLog>, String> {
    daemon()?
        .api_request_logs(100)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_relay_events() -> Result<Vec<codex_companion_core::RelayEvent>, String> {
    Ok(daemon()?.relay_events())
}

#[tauri::command]
fn get_provider_refresh_progress() -> Result<codex_companion_core::ProviderRefreshProgress, String>
{
    Ok(daemon()?.provider_refresh_progress())
}

#[tauri::command]
fn get_provider_import_progress() -> Result<codex_companion_core::ProviderImportProgress, String> {
    Ok(daemon()?.provider_import_progress())
}

#[tauri::command]
fn get_diagnostic_info() -> Result<codex_companion_core::DiagnosticInfo, String> {
    Ok(daemon()?.diagnostic_info())
}

#[tauri::command]
fn clear_diagnostic_logs() -> Result<usize, String> {
    daemon()?
        .clear_diagnostic_logs()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_diagnostic_directory() -> Result<bool, String> {
    daemon()?
        .open_diagnostic_directory()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn report_frontend_error(
    message: String,
    stack: Option<String>,
    component_stack: Option<String>,
) -> Result<(), String> {
    daemon()?
        .report_frontend_error(&message, stack.as_deref(), component_stack.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn create_api_client(
    input: ApiClientCreate,
) -> Result<codex_companion_core::ApiClientSecret, String> {
    daemon()?
        .create_api_client(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_api_client(input: ApiClientUpdate) -> Result<codex_companion_core::ApiClient, String> {
    daemon()?
        .update_api_client(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn rotate_api_client_key(id: String) -> Result<codex_companion_core::ApiClientSecret, String> {
    daemon()?
        .rotate_api_client_key(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_api_client(id: String) -> Result<bool, String> {
    daemon()?
        .delete_api_client(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn clear_api_request_logs() -> Result<usize, String> {
    daemon()?
        .clear_api_request_logs()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_relay_settings(
    input: RelaySettingsUpdate,
) -> Result<codex_companion_core::RelayConfig, String> {
    daemon()?
        .update_relay_settings(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn api_service_self_test() -> Result<codex_companion_core::ApiServiceSelfTest, String> {
    Ok(daemon()?.api_service_self_test().await)
}

#[tauri::command]
fn install(codex_dir: Option<String>) -> Result<codex_companion_core::CodexInstallStatus, String> {
    daemon()?
        .install(codex_dir.map(PathBuf::from))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn uninstall(
    codex_dir: Option<String>,
) -> Result<codex_companion_core::CodexInstallStatus, String> {
    daemon()?
        .uninstall(codex_dir.map(PathBuf::from))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn add_provider(input: ProviderUpsert) -> Result<codex_companion_core::ProviderConfig, String> {
    daemon()?
        .add_provider(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_api_key_provider(
    input: ApiKeyProviderUpdate,
) -> Result<codex_companion_core::ProviderConfig, String> {
    daemon()?
        .update_api_key_provider(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn export_provider_json(
    id: String,
    format: Option<ProviderExportFormat>,
) -> Result<codex_companion_provider::ProviderExportOutput, String> {
    daemon()?
        .export_provider_json(&id, format)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_provider_json(
    json_text: String,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<codex_companion_provider::ProviderImportOutcome, String> {
    daemon()?
        .import_provider_json(&json_text, provider_id, provider_name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_provider_json_many(
    json_text: String,
    provider_id: Option<String>,
    provider_name: Option<String>,
    add_to_group_id: Option<String>,
) -> Result<codex_companion_provider::ProviderImportBatchReport, String> {
    daemon()?
        .import_provider_json_many(&json_text, provider_id, provider_name, add_to_group_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn review_provider_json_many(
    json_text: String,
    provider_id: Option<String>,
    provider_name: Option<String>,
) -> Result<codex_companion_provider::ProviderImportReviewReport, String> {
    daemon()?
        .review_provider_json_many(&json_text, provider_id, provider_name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_api_key_provider(
    input: ApiKeyProviderImportRequest,
) -> Result<codex_companion_provider::ProviderImportOutcome, String> {
    daemon()?
        .import_api_key_provider_request(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_local_codex_provider(
    codex_dir: Option<String>,
) -> Result<codex_companion_provider::ProviderImportOutcome, String> {
    daemon()?
        .import_local_codex_provider(codex_dir.map(PathBuf::from))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn remove_provider(id: String) -> Result<bool, String> {
    daemon()?
        .remove_provider(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn test_provider(id: String) -> Result<(), String> {
    daemon()?.test_provider(&id).await
}

#[tauri::command]
async fn refresh_provider(id: String) -> Result<codex_companion_core::ProviderHealth, String> {
    daemon()?
        .refresh_provider(&id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn refresh_all_providers() -> Result<Vec<codex_companion_core::ProviderHealth>, String> {
    daemon()?
        .refresh_all_providers()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn upsert_group(input: GroupUpsert) -> Result<codex_companion_core::ProviderGroup, String> {
    daemon()?
        .upsert_group(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn use_group(id: String) -> Result<codex_companion_core::ProviderGroup, String> {
    daemon()?.use_group(&id).map_err(|error| error.to_string())
}

#[tauri::command]
fn request_priority_failback(
    id: String,
    provider_id: String,
) -> Result<codex_companion_core::ProviderGroup, String> {
    daemon()?
        .request_priority_failback(&id, &provider_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn launch_group(
    id: String,
    codex_dir: Option<String>,
) -> Result<codex_companion_core::CodexLaunchOutcome, String> {
    daemon()?
        .launch_group(&id, codex_dir.map(PathBuf::from))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn launch_provider(
    id: String,
    codex_dir: Option<String>,
    mode: Option<ProviderLaunchMode>,
) -> Result<codex_companion_core::CodexLaunchOutcome, String> {
    daemon()?
        .launch_provider_with_mode(
            &id,
            codex_dir.map(PathBuf::from),
            mode.unwrap_or(ProviderLaunchMode::Auto),
        )
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn preview_cli_command(input: codex_companion_core::CliLaunchRequest) -> Result<String, String> {
    daemon()?
        .preview_cli_command(&input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn launch_cli(
    input: codex_companion_core::CliLaunchRequest,
) -> Result<codex_companion_core::CliLaunchOutcome, String> {
    daemon()?
        .launch_cli(input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn repair(
    history: bool,
    plugins: bool,
    dry_run: bool,
    codex_dir: Option<String>,
    target_provider_id: Option<String>,
) -> Result<codex_companion_core::RepairOutcome, String> {
    let codex_dir = match codex_dir.filter(|value| !value.trim().is_empty()) {
        Some(value) => PathBuf::from(value),
        None => default_codex_dir().map_err(|error| error.to_string())?,
    };
    let target_provider_id = target_provider_id.filter(|value| !value.trim().is_empty());
    tauri::async_runtime::spawn_blocking(move || {
        daemon()?
            .repair(RepairOptions {
                codex_dir,
                history,
                plugins,
                dry_run,
                target_provider_id,
            })
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn set_theme(theme: ThemeMode) -> Result<ThemeMode, String> {
    daemon()?
        .set_theme(theme)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_provider_view_mode(mode: ProviderViewMode) -> Result<ProviderViewMode, String> {
    daemon()?
        .set_provider_view_mode(mode)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_preserve_official_codex_auth(preserve: bool) -> Result<bool, String> {
    daemon()?
        .set_preserve_official_codex_auth(preserve)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_token_usage_refresh_interval(seconds: u64) -> Result<u64, String> {
    daemon()?
        .set_token_usage_refresh_interval(seconds)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_provider_launch_mode(
    provider_id: String,
    mode: ProviderLaunchMode,
) -> Result<ProviderLaunchMode, String> {
    daemon()?
        .set_provider_launch_mode(provider_id, mode)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reset_app_settings() -> Result<codex_companion_core::AppSettings, String> {
    daemon()?
        .reset_app_settings()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_token_usage(
    codex_dir: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
    rebuild: Option<bool>,
) -> Result<codex_companion_core::TokenUsageSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = match codex_dir.filter(|value| !value.trim().is_empty()) {
            Some(value) => PathBuf::from(value),
            None => default_codex_dir().map_err(|error| error.to_string())?,
        };
        daemon()?
            .token_usage_filtered(
                codex_dir,
                start_date.as_deref(),
                end_date.as_deref(),
                provider_id.as_deref(),
                model.as_deref(),
                rebuild.unwrap_or(false),
            )
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn get_token_usage_sync_status() -> Result<codex_companion_core::TokenUsageSyncStatus, String> {
    Ok(daemon()?.token_usage_sync_status())
}

#[tauri::command]
async fn get_session_page(
    codex_dir: Option<String>,
    query: Option<String>,
    limit: Option<usize>,
    rebuild: Option<bool>,
) -> Result<codex_companion_core::SessionPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = match codex_dir.filter(|value| !value.trim().is_empty()) {
            Some(value) => PathBuf::from(value),
            None => default_codex_dir().map_err(|error| error.to_string())?,
        };
        daemon()?
            .session_page(
                codex_dir,
                query.as_deref(),
                limit.unwrap_or(50),
                rebuild.unwrap_or(false),
            )
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let labels = tray_menu_labels();
            let tray_runtime_status = MenuItemBuilder::with_id(TRAY_STATUS_ID, &labels.runtime)
                .enabled(false)
                .build(app)?;
            let tray_route_status = MenuItemBuilder::with_id(TRAY_ROUTE_ID, &labels.route)
                .enabled(false)
                .build(app)?;
            let tray_launch = MenuItemBuilder::with_id(TRAY_LAUNCH_ID, &labels.launch)
                .enabled(labels.can_launch)
                .build(app)?;
            let page_menu = SubmenuBuilder::new(app, "打开页面")
                .text(TRAY_DASHBOARD_ID, "总览")
                .text(TRAY_PROVIDERS_ID, "账号")
                .text(TRAY_GROUPS_ID, "分组")
                .text(TRAY_RELAY_ID, "转发")
                .text(TRAY_TOKEN_ID, "用量")
                .text(TRAY_SESSIONS_ID, "会话")
                .text(TRAY_REPAIR_ID, "修复")
                .text(TRAY_SETTINGS_ID, "设置")
                .build()?;
            let tray_menu = MenuBuilder::new(app)
                .item(&tray_runtime_status)
                .item(&tray_route_status)
                .separator()
                .text(TRAY_OPEN_ID, "打开 Codex Companion")
                .item(&page_menu)
                .separator()
                .item(&tray_launch)
                .text(TRAY_REFRESH_ID, "刷新账号状态")
                .text(TRAY_LOGS_ID, "打开诊断日志")
                .separator()
                .text(TRAY_HIDE_ID, "隐藏到托盘")
                .text(TRAY_QUIT_ID, "退出 Codex Companion")
                .build()?;
            let mut tray = TrayIconBuilder::with_id(TRAY_ICON_ID)
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .tooltip("Codex Companion")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    TRAY_OPEN_ID => show_main_window(app),
                    TRAY_HIDE_ID => hide_main_window(app),
                    TRAY_QUIT_ID => app.exit(0),
                    TRAY_LAUNCH_ID => {
                        emit_tray_action(app, "launch-active-group", false);
                    }
                    TRAY_REFRESH_ID => {
                        emit_tray_action(app, "refresh-providers", false);
                    }
                    TRAY_LOGS_ID => open_tray_diagnostics(),
                    TRAY_DASHBOARD_ID => emit_tray_action(app, "navigate:dashboard", true),
                    TRAY_PROVIDERS_ID => emit_tray_action(app, "navigate:providers", true),
                    TRAY_GROUPS_ID => emit_tray_action(app, "navigate:groups", true),
                    TRAY_RELAY_ID => emit_tray_action(app, "navigate:relay", true),
                    TRAY_TOKEN_ID => emit_tray_action(app, "navigate:token", true),
                    TRAY_SESSIONS_ID => emit_tray_action(app, "navigate:sessions", true),
                    TRAY_REPAIR_ID => emit_tray_action(app, "navigate:repair", true),
                    TRAY_SETTINGS_ID => emit_tray_action(app, "navigate:settings", true),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            let runtime_status_for_refresh = tray_runtime_status.clone();
            let route_status_for_refresh = tray_route_status.clone();
            let launch_for_refresh = tray_launch.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let labels = tray_menu_labels();
                    let _ = runtime_status_for_refresh.set_text(labels.runtime);
                    let _ = route_status_for_refresh.set_text(labels.route);
                    let _ = launch_for_refresh.set_text(labels.launch);
                    let _ = launch_for_refresh.set_enabled(labels.can_launch);
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            });

            tauri::async_runtime::spawn(async {
                let mut retry_delay = Duration::from_secs(1);
                loop {
                    let daemon = match CompanionDaemon::default() {
                        Ok(daemon) => daemon,
                        Err(error) => {
                            eprintln!("Codex Companion daemon init failed; retrying: {error}");
                            tokio::time::sleep(retry_delay).await;
                            retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                            continue;
                        }
                    };
                    let started_at = Instant::now();
                    if let Err(error) = daemon.start_relay().await {
                        eprintln!("Codex Companion relay stopped; retrying: {error}");
                    }
                    let had_stable_run = started_at.elapsed() >= Duration::from_secs(60);
                    if had_stable_run {
                        retry_delay = Duration::from_secs(1);
                    }
                    tokio::time::sleep(retry_delay).await;
                    if !had_stable_run {
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == MAIN_WINDOW_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            get_api_service_snapshot,
            get_api_request_logs,
            get_relay_events,
            get_provider_refresh_progress,
            get_provider_import_progress,
            get_diagnostic_info,
            clear_diagnostic_logs,
            open_diagnostic_directory,
            report_frontend_error,
            create_api_client,
            update_api_client,
            rotate_api_client_key,
            delete_api_client,
            clear_api_request_logs,
            update_relay_settings,
            api_service_self_test,
            install,
            uninstall,
            add_provider,
            update_api_key_provider,
            export_provider_json,
            import_provider_json,
            import_provider_json_many,
            review_provider_json_many,
            import_api_key_provider,
            import_local_codex_provider,
            remove_provider,
            test_provider,
            refresh_provider,
            refresh_all_providers,
            upsert_group,
            use_group,
            request_priority_failback,
            launch_group,
            launch_provider,
            preview_cli_command,
            launch_cli,
            repair,
            set_theme,
            set_provider_view_mode,
            set_preserve_official_codex_auth,
            set_token_usage_refresh_interval,
            set_provider_launch_mode,
            reset_app_settings,
            get_token_usage,
            get_token_usage_sync_status,
            get_session_page
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Companion");
}
