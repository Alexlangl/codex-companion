use codex_companion_core::{
    default_codex_dir, ApiClientCreate, ApiClientUpdate, ProviderLaunchMode, ProviderViewMode,
    RelaySettingsUpdate, RepairOptions, ThemeMode,
};
use codex_companion_daemon::CompanionDaemon;
use codex_companion_provider::{
    ApiKeyProviderImportRequest, ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat,
    ProviderUpsert,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
        .setup(|_app| {
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
            import_api_key_provider,
            import_local_codex_provider,
            remove_provider,
            test_provider,
            refresh_provider,
            refresh_all_providers,
            upsert_group,
            use_group,
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
