mod account_refresh;
mod agent_identity;
mod auth;
mod codex_oauth;
mod export;
mod groups;
mod import;
mod refresh;
mod registry;
mod types;
mod validate;

use codex_companion_core::{atomic_write_private_file, CompanionError, Result};
use std::{fs, path::Path};

pub use account_refresh::{refresh_official_codex_account, test_configured_usage_query};
pub use auth::{resolve_auth_token, resolve_chatgpt_account_id};
pub use codex_oauth::{ensure_codex_auth_snapshot, load_codex_auth_snapshot, CodexAuthSnapshot};
pub use export::export_provider_json;
pub use groups::{
    active_group, filter_available_providers, request_priority_failback, selected_providers,
    selected_providers_for_group, set_group_order, upsert_group, use_group,
};
pub use import::{
    import_api_key_provider, import_api_key_provider_request, import_local_codex_provider,
    import_provider_json, import_provider_json_many, parse_provider_import_draft,
    provider_import_progress, review_provider_json_many, ApiKeyProviderImportRequest,
};
pub use refresh::{refresh_provider_status, test_provider};
pub use registry::{add_provider, list_providers, remove_provider, update_api_key_provider};
pub use types::{
    ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat, ProviderExportOutput,
    ProviderImportBatchReport, ProviderImportDraft, ProviderImportFailure, ProviderImportOutcome,
    ProviderImportReviewItem, ProviderImportReviewReport, ProviderUpsert,
    ProviderUsageQueryTestInput, ProviderUsageQueryUpdate,
};

pub(crate) fn write_private_auth_file(path: &Path, contents: &str) -> Result<()> {
    atomic_write_private_file(path, contents.as_bytes())
}

pub(crate) fn persist_with_private_auth_file<T>(
    path: &Path,
    contents: &str,
    persist: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_private_auth_file_lock(path, || {
        let previous = if path.exists() {
            Some(fs::read(path).map_err(|source| CompanionError::io(path, source))?)
        } else {
            None
        };
        if let Err(error) = write_private_auth_file(path, contents) {
            return match restore_private_auth_file(path, previous.as_deref()) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CompanionError::InvalidConfig(format!(
                    "credential 写入失败: {error}；回滚也失败: {rollback_error}"
                ))),
            };
        }
        match persist() {
            Ok(value) => Ok(value),
            Err(error) => match restore_private_auth_file(path, previous.as_deref()) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CompanionError::InvalidConfig(format!(
                    "provider 保存失败: {error}；credential 回滚也失败: {rollback_error}"
                ))),
            },
        }
    })
}

pub(crate) fn persist_with_private_auth_file_removal<T>(
    path: &Path,
    persist: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_private_auth_file_lock(path, || {
        let previous = if path.exists() {
            Some(fs::read(path).map_err(|source| CompanionError::io(path, source))?)
        } else {
            None
        };
        if path.exists() {
            fs::remove_file(path).map_err(|source| CompanionError::io(path, source))?;
        }
        match persist() {
            Ok(value) => Ok(value),
            Err(error) => match restore_private_auth_file(path, previous.as_deref()) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(CompanionError::InvalidConfig(format!(
                    "provider 保存失败: {error}；credential 回滚也失败: {rollback_error}"
                ))),
            },
        }
    })
}

pub(crate) fn with_private_auth_file_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _guard = lock_private_auth_file(path)?;
    operation()
}

fn lock_private_auth_file(path: &Path) -> Result<fs::File> {
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    fs::create_dir_all(&parent).map_err(|source| CompanionError::io(&parent, source))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("credential");
    let lock_path = parent.join(format!(".{file_name}.lock"));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|source| CompanionError::io(&lock_path, source))?;
    lock_file
        .lock()
        .map_err(|source| CompanionError::io(&lock_path, source))?;
    Ok(lock_file)
}

fn restore_private_auth_file(path: &Path, previous: Option<&[u8]>) -> Result<()> {
    match previous {
        Some(contents) => atomic_write_private_file(path, contents),
        None if path.exists() => {
            fs::remove_file(path).map_err(|source| CompanionError::io(path, source))
        }
        None => Ok(()),
    }
}
pub use agent_identity::{
    ensure_agent_identity_authorization, is_agent_identity_task_invalid,
    provider_uses_agent_identity, redact_agent_identity_body, AgentIdentityAuthorization,
};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn private_auth_file_is_owner_only_for_new_and_overwritten_files() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");

        write_private_auth_file(&path, r#"{"k":"v"}"#).expect("write new file");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "新建文件必须在创建时就是 0600");

        // 覆盖已存在的宽权限文件时，写入密文前必须先收紧到 0600。
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosen");
        write_private_auth_file(&path, r#"{"k":"v2"}"#).expect("overwrite");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "覆盖已有文件也必须收紧到 0600");
        assert_eq!(fs::read_to_string(&path).expect("read"), r#"{"k":"v2"}"#);
    }

    #[test]
    fn private_auth_transaction_lock_covers_provider_persistence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("auth.json");
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_path = path.clone();
        let first = std::thread::spawn(move || {
            persist_with_private_auth_file(&first_path, "first", || {
                first_entered_tx.send(()).expect("signal first");
                release_first_rx.recv().expect("release first");
                Ok(())
            })
            .expect("first transaction");
        });
        first_entered_rx.recv().expect("first entered");

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            second_started_tx.send(()).expect("signal second start");
            persist_with_private_auth_file(&second_path, "second", || {
                second_entered_tx.send(()).expect("signal second");
                Ok(())
            })
            .expect("second transaction");
        });
        second_started_rx.recv().expect("second started");

        let second_was_blocked = matches!(
            second_entered_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        release_first_tx
            .send(())
            .expect("release first transaction");
        first.join().expect("join first");
        second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second entered after release");
        second.join().expect("join second");

        assert!(second_was_blocked);
        assert_eq!(fs::read_to_string(path).expect("final auth"), "second");
    }
}
