mod constants;
mod diagnostics;
mod error;
mod events;
mod paths;
mod private_file;
mod provider_url;
mod store;
mod types;

pub use constants::*;
pub use diagnostics::*;
pub use error::{CompanionError, Result};
pub use events::now_event;
pub use paths::{default_codex_dir, default_config_path, default_data_dir};
pub use private_file::atomic_write_private_file;
pub use provider_url::{
    provider_api_base_url, provider_base_url_is_endpoint, provider_endpoint_is_chat_completions,
};
pub use store::{ensure_default_group, ConfigStore};
pub use types::*;
