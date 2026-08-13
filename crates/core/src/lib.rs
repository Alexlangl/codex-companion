mod auth;
mod constants;
mod diagnostics;
mod error;
mod events;
mod http_client;
mod paths;
mod private_file;
mod provider_url;
mod store;
mod types;

pub use auth::{
    official_access_token_from_auth_json, official_auth_mode_from_account,
    official_auth_mode_from_auth_json, parse_official_auth_mode, provider_direct_auth_ref,
    provider_relay_auth_ref, OfficialAuthMode, COMPANION_OFFICIAL_AUTH_MODE_FIELD,
};
pub use constants::*;
pub use diagnostics::*;
pub use error::{CompanionError, Result};
pub use events::now_event;
pub use http_client::http_client_builder;
pub use paths::{default_codex_dir, default_config_path, default_data_dir};
pub use private_file::atomic_write_private_file;
pub use provider_url::{
    provider_api_base_url, provider_base_url_is_endpoint, provider_endpoint_is_chat_completions,
};
pub use store::{ensure_default_group, ConfigStore};
pub use types::*;
