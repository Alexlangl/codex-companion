use codex_companion_core::{
    default_refresh_interval_seconds, GroupPolicy, ProviderAccountInfo, ProviderConfig,
    ProviderKind, ProviderUsageQueryTemplate,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const OFFICIAL_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpsert {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub websocket_url: Option<String>,
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
pub struct ProviderUsageQueryUpdate {
    pub enabled: bool,
    #[serde(default = "default_usage_query_template")]
    pub template: ProviderUsageQueryTemplate,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

fn default_usage_query_template() -> ProviderUsageQueryTemplate {
    ProviderUsageQueryTemplate::NewApi
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyProviderUpdate {
    pub id: String,
    #[serde(default)]
    pub provider_display_name: Option<String>,
    pub provider_name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub websocket_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub env_var: Option<String>,
    #[serde(default = "default_refresh_interval_seconds")]
    pub refresh_interval_seconds: u64,
    #[serde(default)]
    pub usage_query: Option<ProviderUsageQueryUpdate>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderExportFormat {
    CodexCompanion,
    Sub2api,
    Cpa,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderExportOutput {
    pub file_name_base: String,
    pub json_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupUpsert {
    pub id: String,
    pub name: String,
    pub policy: GroupPolicy,
    pub provider_order: Vec<String>,
    #[serde(default)]
    pub provider_weights: BTreeMap<String, u16>,
    pub fallback_enabled: bool,
    #[serde(default)]
    pub priority_failback_interval_seconds: u64,
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
    pub user_id: Option<String>,
    pub model: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportFailure {
    pub index: usize,
    pub label: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportReviewItem {
    pub index: usize,
    pub label: String,
    pub provider_id: String,
    pub provider_name: String,
    pub provider_kind: ProviderKind,
    pub import_kind: String,
    pub credential_kind: String,
    pub base_url: String,
    pub websocket_url: Option<String>,
    pub model: Option<String>,
    pub will_overwrite: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportReviewReport {
    pub total: usize,
    pub ready: Vec<ProviderImportReviewItem>,
    pub failed: Vec<ProviderImportFailure>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportBatchReport {
    pub total: usize,
    pub succeeded: Vec<ProviderImportOutcome>,
    pub failed: Vec<ProviderImportFailure>,
    pub added_to_group: Vec<String>,
}
