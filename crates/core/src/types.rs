use crate::constants::{DEFAULT_GROUP_ID, DEFAULT_RELAY_HOST, DEFAULT_RELAY_PORT};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::Light
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderViewMode {
    Compact,
    Cards,
}

impl Default for ProviderViewMode {
    fn default() -> Self {
        Self::Compact
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLaunchMode {
    Auto,
    Direct,
    Relay,
}

impl Default for ProviderLaunchMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OfficialCodex,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    RelayProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupPolicy {
    PriorityFallback,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatusKind {
    Unknown,
    Healthy,
    Degraded,
    Cooldown,
    QuotaExhausted,
    RateLimited,
    AuthFailed,
    ModelMissing,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthFailureKind {
    AuthFailed,
    RateLimited,
    QuotaExhausted,
    ModelMissing,
    UpstreamFailed,
    NetworkFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    pub host: String,
    pub port: u16,
    pub active_group_id: String,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_RELAY_HOST.to_string(),
            port: DEFAULT_RELAY_PORT,
            active_group_id: DEFAULT_GROUP_ID.to_string(),
        }
    }
}

impl RelayConfig {
    pub fn base_url(&self) -> String {
        format!("http://{}:{}/v1", self.host, self.port)
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountInfo {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub team_name: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub subscription_type: Option<String>,
    pub subscription_status: Option<String>,
    pub quota_label: Option<String>,
    pub quota_percent: Option<f64>,
    pub quota_reset_at: Option<String>,
    #[serde(default)]
    pub quota_windows: Vec<ProviderQuotaWindow>,
    pub usage_total: Option<f64>,
    pub usage_used: Option<f64>,
    pub usage_available: Option<f64>,
    pub valid_until: Option<String>,
    pub last_refresh_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuotaWindow {
    pub label: String,
    pub remaining_percent: f64,
    pub reset_at: Option<String>,
    pub window_minutes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderGroup {
    pub id: String,
    pub name: String,
    pub policy: GroupPolicy,
    pub provider_order: Vec<String>,
    pub fallback_enabled: bool,
}

impl ProviderGroup {
    pub fn default_group() -> Self {
        Self {
            id: DEFAULT_GROUP_ID.to_string(),
            name: "Default".to_string(),
            policy: GroupPolicy::PriorityFallback,
            provider_order: Vec::new(),
            fallback_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealth {
    pub status: HealthStatusKind,
    pub last_checked: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_failure_kind: Option<HealthFailureKind>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub failure_count: u32,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            status: HealthStatusKind::Unknown,
            last_checked: None,
            last_success: None,
            last_error: None,
            last_failure_kind: None,
            cooldown_until: None,
            failure_count: 0,
        }
    }
}

pub fn default_refresh_interval_seconds() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub provider_view_mode: ProviderViewMode,
    #[serde(default)]
    pub provider_launch_modes: BTreeMap<String, ProviderLaunchMode>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Light,
            provider_view_mode: ProviderViewMode::Compact,
            provider_launch_modes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionConfig {
    pub relay: RelayConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub groups: BTreeMap<String, ProviderGroup>,
    pub health: BTreeMap<String, ProviderHealth>,
    pub app: AppSettings,
}

impl Default for CompanionConfig {
    fn default() -> Self {
        let mut groups = BTreeMap::new();
        groups.insert(DEFAULT_GROUP_ID.to_string(), ProviderGroup::default_group());
        Self {
            relay: RelayConfig::default(),
            providers: BTreeMap::new(),
            groups,
            health: BTreeMap::new(),
            app: AppSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionStatus {
    pub config: CompanionConfig,
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    pub relay_base_url: String,
    pub active_group: Option<ProviderGroup>,
    pub active_providers: Vec<ProviderConfig>,
    pub codex: CodexInstallStatus,
    pub recent_events: Vec<RelayEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexInstallStatus {
    pub codex_dir: PathBuf,
    pub config_path: PathBuf,
    pub installed: bool,
    pub model_provider: Option<String>,
    pub companion_base_url: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexLaunchMode {
    GroupRelay,
    ProviderDirect,
    ProviderRelay,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexLaunchOutcome {
    pub mode: CodexLaunchMode,
    pub target_id: String,
    pub target_provider_id: String,
    pub codex: CodexInstallStatus,
    pub repair: RepairOutcome,
    pub codex_started: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub provider_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageSummary {
    pub codex_dir: PathBuf,
    pub files_scanned: usize,
    pub sessions: usize,
    pub events: usize,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub by_day: Vec<TokenUsageBucket>,
    pub by_model: Vec<TokenUsageBucket>,
    pub by_provider: Vec<TokenUsageBucket>,
    pub recent_events: Vec<TokenUsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBucket {
    pub key: String,
    pub events: usize,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageEvent {
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub model: String,
    pub provider_id: Option<String>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOptions {
    pub codex_dir: PathBuf,
    pub history: bool,
    pub plugins: bool,
    pub dry_run: bool,
    #[serde(default = "default_repair_target_provider_id")]
    pub target_provider_id: String,
}

pub fn default_repair_target_provider_id() -> String {
    crate::constants::COMPANION_PROVIDER_ID.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairPlan {
    pub codex_dir: PathBuf,
    pub target_provider_id: String,
    pub history_files: usize,
    pub plugin_files: usize,
    pub state_rows: usize,
    pub source_provider_ids: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOutcome {
    pub plan: RepairPlan,
    pub backup_root: Option<PathBuf>,
    pub migrated_history_files: usize,
    pub migrated_history_lines: usize,
    pub migrated_plugin_files: usize,
    pub migrated_state_rows: usize,
    pub skipped_reason: Option<String>,
}
