use crate::constants::{DEFAULT_GROUP_ID, DEFAULT_RELAY_HOST, DEFAULT_RELAY_PORT};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderViewMode {
    #[default]
    Compact,
    Cards,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLaunchMode {
    #[default]
    Auto,
    Direct,
    Relay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OfficialCodex,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    RelayProvider,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSourceKind {
    LocalCache,
    OfficialOauth,
    Relay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSourceStatus {
    Available,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMatrixSource {
    pub id: String,
    pub name: String,
    pub kind: ModelSourceKind,
    pub provider_id: Option<String>,
    pub active_group: bool,
    pub status: ModelSourceStatus,
    pub model_count: usize,
    pub fetched_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMatrixModel {
    pub id: String,
    pub display_name: String,
    pub source_ids: Vec<String>,
    pub reasoning_efforts: Vec<String>,
    pub multi_agent_version: Option<String>,
    pub ultra_capable: bool,
    pub visibility: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMatrixSnapshot {
    pub generated_at: DateTime<Utc>,
    pub sources: Vec<ModelMatrixSource>,
    pub models: Vec<ModelMatrixModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupPolicy {
    PriorityFallback,
    RoundRobin,
    Random,
    Weighted,
    LeastLoaded,
    Manual,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    #[default]
    Auto,
    Terminal,
    ITerm2,
    PowerShell,
    Pwsh,
    WindowsTerminal,
    Cmd,
    Shell,
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
    RequestRejected,
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
    #[serde(default)]
    pub require_api_key: bool,
    #[serde(default)]
    pub retry_budget: u16,
    #[serde(default = "default_model_cooldown_seconds")]
    pub model_cooldown_seconds: u64,
    #[serde(default = "default_session_affinity_ttl_seconds")]
    pub session_affinity_ttl_seconds: u64,
    #[serde(default = "default_request_log_retention_days")]
    pub request_log_retention_days: u16,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_RELAY_HOST.to_string(),
            port: DEFAULT_RELAY_PORT,
            active_group_id: DEFAULT_GROUP_ID.to_string(),
            require_api_key: false,
            retry_budget: 0,
            model_cooldown_seconds: default_model_cooldown_seconds(),
            session_affinity_ttl_seconds: default_session_affinity_ttl_seconds(),
            request_log_retention_days: default_request_log_retention_days(),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageQuery {
    pub template: ProviderUsageQueryTemplate,
    pub base_url: String,
    #[serde(default)]
    pub script: String,
    #[serde(default = "default_usage_query_timeout_seconds")]
    pub timeout_seconds: u64,
}

pub fn default_usage_query_timeout_seconds() -> u64 {
    10
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageQueryTemplate {
    General,
    NewApi,
    OpenRouter,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountInfo {
    #[serde(default)]
    pub auth_mode: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub team_name: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    #[serde(default)]
    pub usage_query: Option<ProviderUsageQuery>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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
    #[serde(default)]
    pub provider_weights: BTreeMap<String, u16>,
    pub fallback_enabled: bool,
    #[serde(default)]
    pub priority_failback_interval_seconds: u64,
    #[serde(default)]
    pub priority_failback_revision: u64,
    #[serde(default)]
    pub priority_failback_target_provider_id: Option<String>,
}

impl ProviderGroup {
    pub fn default_group() -> Self {
        Self {
            id: DEFAULT_GROUP_ID.to_string(),
            name: "Default".to_string(),
            policy: GroupPolicy::PriorityFallback,
            provider_order: Vec::new(),
            provider_weights: BTreeMap::new(),
            fallback_enabled: true,
            priority_failback_interval_seconds: 0,
            priority_failback_revision: 0,
            priority_failback_target_provider_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealth {
    pub status: HealthStatusKind,
    pub last_checked: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_refresh_attempt: Option<DateTime<Utc>>,
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
            last_refresh_attempt: None,
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

pub fn default_model_cooldown_seconds() -> u64 {
    300
}

pub fn default_session_affinity_ttl_seconds() -> u64 {
    3600
}

pub fn default_request_log_retention_days() -> u16 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaySettingsUpdate {
    pub require_api_key: bool,
    pub retry_budget: u16,
    pub model_cooldown_seconds: u64,
    pub session_affinity_ttl_seconds: u64,
    pub request_log_retention_days: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClient {
    pub id: String,
    pub name: String,
    pub key_prefix: String,
    pub allowed_models: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_count: u64,
    #[serde(default)]
    pub usage: ApiClientUsage,
    #[serde(default)]
    pub health: ApiClientHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientUsage {
    pub today: ApiClientPeriodUsage,
    pub week: ApiClientPeriodUsage,
    pub month: ApiClientPeriodUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientPeriodUsage {
    pub requests: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub success_rate: u8,
    pub average_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientHealth {
    pub status: String,
    pub last_request_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientCreate {
    pub name: String,
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientUpdate {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiClientSecret {
    pub client: ApiClient,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequestAttemptLog {
    pub attempt: u16,
    pub provider_id: String,
    pub route_reason: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status_code: Option<u16>,
    pub outcome: String,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequestLog {
    pub request_id: String,
    pub started_at: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub client_id: Option<String>,
    pub client_name: Option<String>,
    pub provider_id: Option<String>,
    pub status_code: Option<u16>,
    pub outcome: String,
    pub attempts: u16,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    #[serde(default)]
    pub attempt_log: Vec<ApiRequestAttemptLog>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCooldown {
    pub provider_id: String,
    pub model: String,
    pub reason: String,
    pub cooldown_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiServiceSnapshot {
    pub clients: Vec<ApiClient>,
    pub recent_requests: Vec<ApiRequestLog>,
    pub model_cooldowns: Vec<ModelCooldown>,
    #[serde(default)]
    pub affinity_bindings: u64,
    #[serde(default)]
    pub pool_health: ApiPoolHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApiPoolHealth {
    pub total: usize,
    pub enabled: usize,
    pub healthy: usize,
    pub degraded: usize,
    pub cooldown: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiServiceSelfTest {
    pub ok: bool,
    pub base_url: String,
    pub latency_ms: u64,
    pub database_ok: bool,
    pub listener_ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRefreshProgress {
    pub active: bool,
    pub completed: usize,
    pub total: usize,
    pub current_provider_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderImportProgress {
    pub active: bool,
    pub completed: usize,
    pub total: usize,
    pub current_label: Option<String>,
    pub succeeded: usize,
    pub failed: usize,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageSyncStatus {
    pub active: bool,
    pub scanned_files: usize,
    pub total_files: usize,
    #[serde(default)]
    pub deferred_files: usize,
    #[serde(default)]
    pub suspected_duplicates: usize,
    pub phase: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub cwd_available: bool,
    pub model: String,
    pub provider_id: Option<String>,
    pub path: PathBuf,
    pub modified_at: DateTime<Utc>,
    pub bytes: u64,
    pub is_subagent: bool,
    pub parent_id: Option<String>,
    pub is_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub total: usize,
    pub query: String,
    pub from_cache: bool,
    pub data_root: PathBuf,
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
    #[serde(default)]
    pub last_codex_launch_mode: Option<CodexLaunchMode>,
    #[serde(default)]
    pub last_codex_target_provider_id: Option<String>,
    #[serde(default)]
    pub codex_restart_required_on_next_relay: bool,
    #[serde(default)]
    pub preserve_official_codex_auth: bool,
    #[serde(default = "default_token_usage_refresh_interval_seconds")]
    pub token_usage_refresh_interval_seconds: u64,
    #[serde(default)]
    pub preferred_terminal: TerminalKind,
    #[serde(default)]
    pub recent_working_directories: Vec<PathBuf>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Light,
            provider_view_mode: ProviderViewMode::Compact,
            provider_launch_modes: BTreeMap::new(),
            last_codex_launch_mode: None,
            last_codex_target_provider_id: None,
            codex_restart_required_on_next_relay: false,
            preserve_official_codex_auth: false,
            token_usage_refresh_interval_seconds: default_token_usage_refresh_interval_seconds(),
            preferred_terminal: TerminalKind::Auto,
            recent_working_directories: Vec::new(),
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
    pub data_roots: DataRootStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataRootStatus {
    pub companion_isolated: bool,
    pub codex_isolated: bool,
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
    pub restart_required: bool,
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
    #[serde(default)]
    pub deferred_files: usize,
    #[serde(default)]
    pub suspected_duplicates: usize,
    #[serde(default)]
    pub cache_version: u32,
    pub sessions: usize,
    pub events: usize,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: TokenCostBreakdown,
    pub priced_events: usize,
    pub unpriced_events: usize,
    pub unpriced_models: Vec<String>,
    pub pricing_as_of: String,
    pub pricing_override_path: Option<PathBuf>,
    pub available_providers: Vec<String>,
    pub available_models: Vec<String>,
    pub by_day: Vec<TokenUsageBucket>,
    pub by_model: Vec<TokenUsageBucket>,
    pub by_provider: Vec<TokenUsageBucket>,
    pub recent_events: Vec<TokenUsageEvent>,
}

pub fn default_token_usage_refresh_interval_seconds() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliLaunchRequest {
    pub working_directory: PathBuf,
    #[serde(default)]
    pub fallback_working_directories: Vec<PathBuf>,
    #[serde(default)]
    pub terminal: TerminalKind,
    #[serde(default)]
    pub resume_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliLaunchOutcome {
    pub command: String,
    pub terminal: TerminalKind,
    pub working_directory: PathBuf,
    pub launched: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticInfo {
    pub log_directory: PathBuf,
    pub current_log_path: PathBuf,
    pub retained_files: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBucket {
    pub key: String,
    pub events: usize,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: TokenCostBreakdown,
    pub priced_events: usize,
    pub unpriced_events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCostBreakdown {
    pub fresh_input_usd: String,
    pub cached_input_usd: String,
    pub cache_write_input_usd: String,
    pub output_usd: String,
    pub total_usd: String,
}

impl Default for TokenCostBreakdown {
    fn default() -> Self {
        Self {
            fresh_input_usd: "0".to_string(),
            cached_input_usd: "0".to_string(),
            cache_write_input_usd: "0".to_string(),
            output_usd: "0".to_string(),
            total_usd: "0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageEvent {
    pub event_id: Option<String>,
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub model: String,
    pub provider_id: Option<String>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost: Option<TokenCostBreakdown>,
    pub pricing_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairOptions {
    pub codex_dir: PathBuf,
    pub history: bool,
    pub plugins: bool,
    pub dry_run: bool,
    #[serde(default)]
    pub target_provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairPlan {
    pub codex_dir: PathBuf,
    pub target_provider_id: String,
    pub history_files: usize,
    pub history_lines: usize,
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

#[cfg(test)]
mod tests {
    use super::ProviderGroup;

    #[test]
    fn legacy_provider_group_defaults_priority_failback_to_disabled() {
        let group: ProviderGroup = serde_json::from_str(
            r#"{
                "id": "default",
                "name": "Default",
                "policy": "priority_fallback",
                "providerOrder": ["a", "b"],
                "fallbackEnabled": true
            }"#,
        )
        .expect("legacy group");

        assert_eq!(group.priority_failback_interval_seconds, 0);
        assert_eq!(group.priority_failback_revision, 0);
        assert_eq!(group.priority_failback_target_provider_id, None);
    }
}
