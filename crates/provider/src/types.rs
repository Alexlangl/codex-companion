use codex_companion_core::{
    default_refresh_interval_seconds, GroupPolicy, ProviderAccountInfo, ProviderConfig,
    ProviderKind,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const OFFICIAL_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5-codex";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpsert {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub auth_ref: Option<String>,
    #[serde(default)]
    pub direct_auth_ref: Option<String>,
    pub model_map: BTreeMap<String, String>,
    pub priority: i32,
    pub enabled: bool,
    #[serde(default = "default_refresh_interval_seconds")]
    pub refresh_interval_seconds: u64,
    #[serde(default)]
    pub account: Option<ProviderAccountInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupUpsert {
    pub id: String,
    pub name: String,
    pub policy: GroupPolicy,
    pub provider_order: Vec<String>,
    pub fallback_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportDraft {
    pub provider_id: String,
    pub provider_name: String,
    pub import_kind: String,
    pub base_url: String,
    pub auth_ref: String,
    pub account_id: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportOutcome {
    pub provider: ProviderConfig,
    pub import_kind: String,
    pub account_id: String,
    pub auth_path: PathBuf,
    pub created: bool,
    pub message: String,
}
