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

use codex_companion_core::{CompanionError, Result};
use std::fs;
use std::path::Path;

pub use account_refresh::refresh_official_codex_account;
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
    provider_import_progress, ApiKeyProviderImportRequest,
};
pub use refresh::{refresh_provider_status, test_provider};
pub use registry::{add_provider, list_providers, remove_provider, update_api_key_provider};
pub use types::{
    ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat, ProviderExportOutput,
    ProviderImportBatchReport, ProviderImportDraft, ProviderImportFailure, ProviderImportOutcome,
    ProviderUpsert,
};

#[cfg(unix)]
pub(crate) fn write_private_auth_file(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // 新建文件时用 mode(0o600) 原子设置权限，避免先按 umask 建文件再 chmod
    // 之间的暴露窗口；覆盖已有文件时 mode 不生效，所以写入密文前先收紧权限。
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|source| CompanionError::io(path, source))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| CompanionError::io(path, source))?;
    file.set_len(0)
        .map_err(|source| CompanionError::io(path, source))?;
    file.write_all(contents.as_bytes())
        .map_err(|source| CompanionError::io(path, source))
}

#[cfg(not(unix))]
pub(crate) fn write_private_auth_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).map_err(|source| CompanionError::io(path, source))
}
pub use agent_identity::{
    ensure_agent_identity_authorization, is_agent_identity_task_invalid,
    provider_uses_agent_identity, redact_agent_identity_body, AgentIdentityAuthorization,
};

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
}
