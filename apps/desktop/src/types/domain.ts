export type ThemeMode = "light" | "dark" | "system";
export type ProviderViewMode = "compact" | "cards";
export type ProviderLaunchMode = "auto" | "direct" | "relay";
export type ProviderKind = "official_codex" | "openai_compatible" | "relay_provider";
export type GroupPolicy = "priority_fallback" | "manual";

export interface RelayConfig {
  host: string;
  port: number;
  activeGroupId: string;
}

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  authRef?: string | null;
  directAuthRef?: string | null;
  modelMap: Record<string, string>;
  priority: number;
  enabled: boolean;
  refreshIntervalSeconds: number;
  account?: ProviderAccountInfo | null;
}

export interface ProviderAccountInfo {
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
  fallbackEnabled: boolean;
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
  sessions: number;
  events: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
  byDay: TokenUsageBucket[];
  byModel: TokenUsageBucket[];
  byProvider: TokenUsageBucket[];
  recentEvents: TokenUsageEvent[];
}

export interface TokenUsageBucket {
  key: string;
  events: number;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export interface TokenUsageEvent {
  timestamp?: string | null;
  sessionId?: string | null;
  model: string;
  providerId?: string | null;
  inputTokens: number;
  cachedInputTokens: number;
  outputTokens: number;
  totalTokens: number;
}

export interface ProviderUpsert {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  authRef?: string | null;
  directAuthRef?: string | null;
  modelMap: Record<string, string>;
  priority: number;
  enabled: boolean;
  refreshIntervalSeconds: number;
  account?: ProviderAccountInfo | null;
}

export interface ProviderImportOutcome {
  provider: ProviderConfig;
  importKind: string;
  accountId: string;
  authPath: string;
  created: boolean;
  message: string;
}

export interface GroupUpsert {
  id: string;
  name: string;
  policy: GroupPolicy;
  providerOrder: string[];
  fallbackEnabled: boolean;
}

export interface RepairOutcome {
  plan: {
    codexDir: string;
    targetProviderId: string;
    historyFiles: number;
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
