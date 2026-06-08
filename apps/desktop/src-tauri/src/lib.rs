use codex_companion_core::{
    default_codex_dir, ProviderKind, ProviderLaunchMode, ProviderViewMode, RepairOptions, ThemeMode,
};
use codex_companion_daemon::CompanionDaemon;
use codex_companion_provider::{
    ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat, ProviderUpsert,
};
use std::path::PathBuf;

fn daemon() -> Result<CompanionDaemon, String> {
    CompanionDaemon::default().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_status() -> Result<codex_companion_core::CompanionStatus, String> {
    daemon()?.status().map_err(|error| error.to_string())
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
) -> Result<Vec<codex_companion_provider::ProviderImportOutcome>, String> {
    daemon()?
        .import_provider_json_many(&json_text, provider_id, provider_name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn import_api_key_provider(
    provider_name: String,
    kind: ProviderKind,
    base_url: String,
    api_key: String,
    env_var: Option<String>,
    model: Option<String>,
    refresh_interval_seconds: Option<u64>,
) -> Result<codex_companion_provider::ProviderImportOutcome, String> {
    daemon()?
        .import_api_key_provider(
            provider_name,
            kind,
            base_url,
            api_key,
            env_var,
            model,
            refresh_interval_seconds,
        )
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
) -> Result<codex_companion_core::TokenUsageSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let codex_dir = match codex_dir.filter(|value| !value.trim().is_empty()) {
            Some(value) => PathBuf::from(value),
            None => default_codex_dir().map_err(|error| error.to_string())?,
        };
        daemon()?
            .token_usage(codex_dir)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            tauri::async_runtime::spawn(async {
                match CompanionDaemon::default() {
                    Ok(daemon) => {
                        daemon.start_background_tasks();
                        if let Err(error) = daemon.start_relay().await {
                            eprintln!("Codex Companion relay stopped: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!("Codex Companion daemon init failed: {error}");
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
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
            repair,
            set_theme,
            set_provider_view_mode,
            set_provider_launch_mode,
            reset_app_settings,
            get_token_usage
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex Companion");
}
