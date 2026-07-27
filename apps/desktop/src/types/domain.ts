export type ThemeMode = "light" | "dark" | "system";
export type ProviderViewMode = "compact" | "cards";
export type ProviderLaunchMode = "auto" | "direct" | "relay";
export type ProviderKind = "official_codex" | "openai_compatible" | "relay_provider";
export type GroupPolicy = "priority_fallback" | "round_robin" | "random" | "weighted" | "least_loaded" | "manual";
export type TerminalKind = "auto" | "terminal" | "i_term2" | "power_shell" | "pwsh" | "windows_terminal" | "cmd" | "shell";

export interface RelayConfig {
  host: string;
  port: number;
  activeGroupId: string;
  requireApiKey: boolean;
  retryBudget: number;
  modelCooldownSeconds: number;
  sessionAffinityTtlSeconds: number;
  requestLogRetentionDays: number;
}

export interface RelaySettingsUpdate {
  requireApiKey: boolean;
  retryBudget: number;
  modelCooldownSeconds: number;
  sessionAffinityTtlSeconds: number;
  requestLogRetentionDays: number;
}

export interface ApiClient {
  id: string;
  name: string;
  keyPrefix: string;
  allowedModels: string[];
  enabled: boolean;
  createdAt: string;
  lastUsedAt?: string | null;
  requestCount: number;
  usage: ApiClientUsage;
  health: ApiClientHealth;
}

export interface ApiClientUsage {
  today: ApiClientPeriodUsage;
  week: ApiClientPeriodUsage;
  month: ApiClientPeriodUsage;
}

export interface ApiClientPeriodUsage {
  requests: number;
  succeeded: number;
  failed: number;
  successRate: number;
  averageLatencyMs?: number | null;
}

export interface ApiClientHealth {
  status: string;
  lastRequestAt?: string | null;
  lastSuccessAt?: string | null;
  lastFailureAt?: string | null;
  consecutiveFailures: number;
}

export interface ApiClientCreate {
  name: string;
  allowedModels: string[];
}

export interface ApiClientUpdate extends ApiClientCreate {
  id: string;
  enabled: boolean;
}

export interface ApiClientSecret {
  client: ApiClient;
  apiKey: string;
}

export interface ApiRequestLog {
  requestId: string;
  startedAt: string;
  method: string;
  path: string;
  model?: string | null;
  clientId?: string | null;
  clientName?: string | null;
  providerId?: string | null;
  statusCode?: number | null;
  outcome: string;
  attempts: number;
  latencyMs?: number | null;
  error?: string | null;
}

export interface ModelCooldown {
  providerId: string;
  model: string;
  reason: string;
  cooldownUntil: string;
}

export interface ApiServiceSnapshot {
  clients: ApiClient[];
  recentRequests: ApiRequestLog[];
  modelCooldowns: ModelCooldown[];
  affinityBindings: number;
  poolHealth: ApiPoolHealth;
}

export interface ApiPoolHealth {
  total: number;
  enabled: number;
  healthy: number;
  degraded: number;
  cooldown: number;
}

export interface ProviderRefreshProgress {
  active: boolean;
  completed: number;
  total: number;
  currentProviderId?: string | null;
  startedAt?: string | null;
  finishedAt?: string | null;
  lastError?: string | null;
}

export interface ProviderImportProgress {
  active: boolean;
  completed: number;
  total: number;
  currentLabel?: string | null;
  succeeded: number;
  failed: number;
  startedAt?: string | null;
  finishedAt?: string | null;
}

export interface TokenUsageSyncStatus {
  active: boolean;
  scannedFiles: number;
  totalFiles: number;
  deferredFiles: number;
  suspectedDuplicates: number;
  phase: string;
  startedAt?: string | null;
  finishedAt?: string | null;
}

export interface SessionSummary {
  id: string;
  title: string;
  cwd?: string | null;
  cwdAvailable: boolean;
  model: string;
  providerId?: string | null;
  path: string;
  modifiedAt: string;
  bytes: number;
  isSubagent: boolean;
  parentId?: string | null;
  isRunning: boolean;
}

export interface SessionPage {
  sessions: SessionSummary[];
  total: number;
  query: string;
  fromCache: boolean;
  dataRoot: string;
}

export interface ApiServiceSelfTest {
  ok: boolean;
  baseUrl: string;
  websocketUrl?: string | null;
  latencyMs: number;
  databaseOk: boolean;
  listenerOk: boolean;
  message: string;
}

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  websocketUrl?: string | null;
  authRef?: string | null;
  directAuthRef?: string | null;
  modelMap: Record<string, string>;
  priority: number;
  enabled: boolean;
  refreshIntervalSeconds: number;
  account?: ProviderAccountInfo | null;
}

export interface ProviderAccountInfo {
  authMode?: string | null;
  displayName?: string | null;
  email?: string | null;
  teamName?: string | null;
  accountId?: string | null;
  userId?: string | null;
  subscriptionType?: string | null;
  subscriptionStatus?: string | null;
  quotaLabel?: string | null;
  quotaPercent?: number | null;
  quotaResetAt?: string | null;
  quotaWindows?: ProviderQuotaWindow[];
  usageTotal?: number | null;
  usageUsed?: number | null;
  usageAvailable?: number | null;
  validUntil?: string | null;
  lastRefreshAt?: string | null;
}

export interface ProviderQuotaWindow {
  label: string;
  remainingPercent: number;
  resetAt?: string | null;
  windowMinutes?: number | null;
}

export interface ProviderHealth {
  status: string;
  lastChecked?: string | null;
  lastSuccess?: string | null;
  lastError?: string | null;
  lastFailureKind?: string | null;
  cooldownUntil?: string | null;
  failureCount: number;
}

export interface ProviderGroup {
  id: string;
  name: string;
  policy: GroupPolicy;
  providerOrder: string[];
  providerWeights: Record<string, number>;
  fallbackEnabled: boolean;
  priorityFailbackIntervalSeconds: number;
  priorityFailbackRevision: number;
  priorityFailbackTargetProviderId?: string | null;
}

export interface CompanionConfig {
  relay: RelayConfig;
  providers: Record<string, ProviderConfig>;
  groups: Record<string, ProviderGroup>;
  health: Record<string, ProviderHealth>;
  app: AppSettings;
}

export interface AppSettings {
  theme: ThemeMode;
  providerViewMode: ProviderViewMode;
  providerLaunchModes: Record<string, ProviderLaunchMode>;
  lastCodexLaunchMode?: CodexLaunchMode | null;
  lastCodexTargetProviderId?: string | null;
  codexRestartRequiredOnNextRelay?: boolean;
  preserveOfficialCodexAuth?: boolean;
  tokenUsageRefreshIntervalSeconds: number;
  preferredTerminal: TerminalKind;
  recentWorkingDirectories: string[];
}

export interface CodexInstallStatus {
  codexDir: string;
  configPath: string;
  installed: boolean;
  modelProvider?: string | null;
  companionBaseUrl: string;
  message: string;
}

export type CodexLaunchMode = "group_relay" | "provider_direct" | "provider_relay";

export interface CodexLaunchOutcome {
  mode: CodexLaunchMode;
  targetId: string;
  targetProviderId: string;
  codex: CodexInstallStatus;
  repair: RepairOutcome;
  restartRequired: boolean;
  codexStarted: boolean;
  message: string;
}

export interface CompanionStatus {
  config: CompanionConfig;
  dataDir: string;
  configPath: string;
  relayBaseUrl: string;
  activeGroup?: ProviderGroup | null;
  activeProviders: ProviderConfig[];
  codex: CodexInstallStatus;
  recentEvents: RelayEvent[];
  dataRoots: DataRootStatus;
}

export interface DataRootStatus {
  companionIsolated: boolean;
  codexIsolated: boolean;
}

export interface RelayEvent {
  timestamp: string;
  kind: string;
  providerId?: string | null;
  message: string;
}

export interface TokenUsageSummary {
  codexDir: string;
  filesScanned: number;
  deferredFiles: number;
  suspectedDuplicates: number;
  cacheVersion: number;
  sessions: number;
  events: number;
  inputTokens: number;
  cachedInputTokens: number;
  cacheWriteInputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cost: TokenCostBreakdown;
  pricedEvents: number;
  unpricedEvents: number;
  unpricedModels: string[];
  pricingAsOf: string;
  pricingOverridePath?: string | null;
  availableProviders: string[];
  availableModels: string[];
  byDay: TokenUsageBucket[];
  byModel: TokenUsageBucket[];
  byProvider: TokenUsageBucket[];
  recentEvents: TokenUsageEvent[];
}

export interface TokenUsageDateRange {
  startDate?: string;
  endDate?: string;
}

export interface TokenUsageQuery extends TokenUsageDateRange {
  providerId?: string;
  model?: string;
  rebuild?: boolean;
}

export interface TokenUsageBucket {
  key: string;
  events: number;
  inputTokens: number;
  cachedInputTokens: number;
  cacheWriteInputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cost: TokenCostBreakdown;
  pricedEvents: number;
  unpricedEvents: number;
}

export interface TokenCostBreakdown {
  freshInputUsd: string;
  cachedInputUsd: string;
  cacheWriteInputUsd: string;
  outputUsd: string;
  totalUsd: string;
}

export interface TokenUsageEvent {
  eventId?: string | null;
  timestamp?: string | null;
  sessionId?: string | null;
  model: string;
  providerId?: string | null;
  inputTokens: number;
  cachedInputTokens: number;
  cacheWriteInputTokens: number;
  outputTokens: number;
  totalTokens: number;
  cost?: TokenCostBreakdown | null;
  pricingModel?: string | null;
}

export interface ProviderUpsert {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  websocketUrl?: string | null;
  authRef?: string | null;
  directAuthRef?: string | null;
  modelMap: Record<string, string>;
  priority: number;
  enabled: boolean;
  refreshIntervalSeconds: number;
  account?: ProviderAccountInfo | null;
}

export interface ApiKeyProviderUpdate {
  id: string;
  providerDisplayName?: string | null;
  providerName: string;
  kind: Extract<ProviderKind, "openai_compatible" | "relay_provider">;
  baseUrl: string;
  websocketUrl?: string | null;
  apiKey?: string | null;
  envVar?: string | null;
  refreshIntervalSeconds: number;
}

export type ProviderExportFormat = "codex_companion" | "sub2api" | "cpa";

export interface ProviderExportOutput {
  fileNameBase: string;
  jsonContent: string;
}

export interface ProviderImportOutcome {
  provider: ProviderConfig;
  importKind: string;
  accountId: string;
  authPath: string;
  created: boolean;
  message: string;
}

export interface ProviderImportFailure {
  index: number;
  label: string;
  message: string;
}

export interface ProviderImportBatchReport {
  total: number;
  succeeded: ProviderImportOutcome[];
  failed: ProviderImportFailure[];
  addedToGroup: string[];
}

export interface GroupUpsert {
  id: string;
  name: string;
  policy: GroupPolicy;
  providerOrder: string[];
  providerWeights: Record<string, number>;
  fallbackEnabled: boolean;
  priorityFailbackIntervalSeconds: number;
}

export interface CliLaunchRequest {
  workingDirectory: string;
  fallbackWorkingDirectories?: string[];
  terminal: TerminalKind;
  resumeSessionId?: string | null;
}

export interface CliLaunchOutcome {
  command: string;
  terminal: TerminalKind;
  workingDirectory: string;
  launched: boolean;
  message: string;
}

export interface DiagnosticInfo {
  logDirectory: string;
  currentLogPath: string;
  retainedFiles: number;
  totalBytes: number;
}

export interface RepairOutcome {
  plan: {
    codexDir: string;
    targetProviderId: string;
    historyFiles: number;
    historyLines: number;
    pluginFiles: number;
    stateRows: number;
    sourceProviderIds: string[];
    dryRun: boolean;
  };
  backupRoot?: string | null;
  migratedHistoryFiles: number;
  migratedHistoryLines: number;
  migratedPluginFiles: number;
  migratedStateRows: number;
  skippedReason?: string | null;
}

export type BusyState = "idle" | "loading" | "saving" | "testing" | "repairing" | "launching";
