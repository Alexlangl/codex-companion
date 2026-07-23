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
    active_group, filter_available_providers, selected_providers, selected_providers_for_group,
    set_group_order, upsert_group, use_group,
};
pub use import::{
    import_api_key_provider, import_local_codex_provider, import_provider_json,
    import_provider_json_many, parse_provider_import_draft, provider_import_progress,
};
pub use refresh::{refresh_provider_status, test_provider};
pub use registry::{add_provider, list_providers, remove_provider, update_api_key_provider};
pub use types::{
    ApiKeyProviderUpdate, GroupUpsert, ProviderExportFormat, ProviderExportOutput,
    ProviderImportBatchReport, ProviderImportDraft, ProviderImportFailure, ProviderImportOutcome,
    ProviderUpsert,
};

pub(crate) fn write_private_auth_file(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).map_err(|source| CompanionError::io(path, source))?;
    harden_auth_file_permissions(path)
}

#[cfg(unix)]
fn harden_auth_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|source| CompanionError::io(path, source))
}

#[cfg(not(unix))]
fn harden_auth_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
pub use agent_identity::{
    ensure_agent_identity_authorization, is_agent_identity_task_invalid,
    provider_uses_agent_identity, redact_agent_identity_body, AgentIdentityAuthorization,
};
