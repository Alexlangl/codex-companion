import { invoke } from "@tauri-apps/api/core";
import { userFacingError } from "./errors";
import type {
  ApiClient,
  ApiClientCreate,
  ApiClientSecret,
  ApiClientUpdate,
  ApiRequestLog,
  ApiServiceSelfTest,
  ApiServiceSnapshot,
  CliLaunchOutcome,
  CliLaunchRequest,
  CodexOAuthStartResponse,
  CodexOAuthStatus,
  DiagnosticInfo,
  ProviderRefreshProgress,
  ApiKeyProviderUpdate,
  AppSettings,
  CodexLaunchOutcome,
  CodexInstallStatus,
  CompanionStatus,
  GroupUpsert,
  ModelMatrixSnapshot,
  ProviderConfig,
  ProviderAccountInfo,
  ProviderExportFormat,
  ProviderExportOutput,
  ProviderHealth,
  ProviderImportOutcome,
  ProviderImportBatchReport,
  ProviderImportReviewItem,
  ProviderImportReviewReport,
  ProviderImportProgress,
  ProviderGroup,
  ProviderKind,
  ProviderLaunchMode,
  ProviderUsageQueryTemplate,
  ProviderUsageQueryTestInput,
  ProviderViewMode,
  ProviderUpsert,
  RepairOutcome,
  RelayConfig,
  RelayEvent,
  RelaySettingsUpdate,
  SessionPage,
  ThemeMode,
  TokenUsageQuery,
} from "../types/domain";
import { isGenericOfficialAccountName } from "./provider-display";
import { providerEndpointIsChatCompletions } from "./provider-url";
import {
  extractQuotaWindows,
  findApiBaseUrl,
  findApiKey,
  findNumber,
  findString,
  isApiKeyJson,
  isNewApiChannelConnection,
  lowestQuotaPercent,
  providerNameFromBaseUrl,
} from "./provider-json";
import { getTokenUsage as getTokenUsageFromRuntime } from "./token-usage-api";

const APP_PREFS_STORAGE_KEY = "codex-companion:app-settings";
const MOCK_HOME_DIR = "/mock-home";
const MOCK_DATA_DIR = `${MOCK_HOME_DIR}/.codex-companion`;
const MOCK_CODEX_DIR = `${MOCK_HOME_DIR}/.codex`;

let mockStatus = createMockStatus();
let mockApiService = createMockApiServiceSnapshot();
let mockOAuthPending: (CodexOAuthStartResponse & { callbackReceived: boolean }) | null = null;
let mockSessionProviderPreferences: Record<string, string> = {};

export function getStatus() {
  if (!isTauri()) return Promise.resolve(mockStatus);
  return invoke<CompanionStatus>("get_status");
}

export function getModelMatrix() {
  if (!isTauri()) return Promise.resolve(structuredClone(createMockModelMatrix()));
  return invoke<ModelMatrixSnapshot>("get_model_matrix");
}

export function getApiServiceSnapshot() {
  if (!isTauri()) return Promise.resolve(structuredClone(mockApiService));
  return invoke<ApiServiceSnapshot>("get_api_service_snapshot");
}

export function getApiRequestLogs() {
  if (!isTauri()) return Promise.resolve(structuredClone(mockApiService.recentRequests));
  return invoke<ApiRequestLog[]>("get_api_request_logs");
}

export function getRelayEvents() {
  if (!isTauri()) return Promise.resolve(structuredClone(mockStatus.recentEvents));
  return invoke<RelayEvent[]>("get_relay_events");
}

export function getProviderRefreshProgress() {
  if (!isTauri()) {
    return Promise.resolve<ProviderRefreshProgress>({
      active: false,
      completed: 0,
      total: 0,
      currentProviderId: null,
      startedAt: null,
      finishedAt: null,
      lastError: null,
    });
  }
  return invoke<ProviderRefreshProgress>("get_provider_refresh_progress");
}

export function getProviderImportProgress() {
  if (!isTauri()) {
    return Promise.resolve<ProviderImportProgress>({
      active: false,
      completed: 0,
      total: 0,
      currentLabel: null,
      succeeded: 0,
      failed: 0,
      startedAt: null,
      finishedAt: null,
    });
  }
  return invoke<ProviderImportProgress>("get_provider_import_progress");
}

export function getDiagnosticInfo() {
  if (!isTauri()) {
    return Promise.resolve<DiagnosticInfo>({
      logDirectory: `${MOCK_DATA_DIR}/logs`,
      currentLogPath: `${MOCK_DATA_DIR}/logs/companion.log.jsonl`,
      retainedFiles: 1,
      totalBytes: 12_480,
    });
  }
  return invoke<DiagnosticInfo>("get_diagnostic_info");
}

export function clearDiagnosticLogs() {
  if (!isTauri()) return Promise.resolve(1);
  return invoke<number>("clear_diagnostic_logs");
}

export function openDiagnosticDirectory() {
  if (!isTauri()) return Promise.resolve(false);
  return invoke<boolean>("open_diagnostic_directory");
}

export function reportFrontendError(
  message: string,
  stack?: string | null,
  componentStack?: string | null,
) {
  if (!isTauri()) return Promise.resolve();
  return invoke<void>("report_frontend_error", {
    message,
    stack: stack ?? null,
    componentStack: componentStack ?? null,
  });
}

export function previewCliCommand(input: CliLaunchRequest) {
  if (!isTauri()) return Promise.resolve(mockCliCommand(input));
  return invoke<string>("preview_cli_command", { input });
}

export function launchCli(input: CliLaunchRequest) {
  if (!isTauri()) {
    return Promise.resolve<CliLaunchOutcome>({
      command: mockCliCommand(input),
      terminal: input.terminal === "auto" ? "terminal" : input.terminal,
      workingDirectory: input.workingDirectory,
      launched: true,
      message: "已在所选终端启动 Codex CLI",
    });
  }
  return invoke<CliLaunchOutcome>("launch_cli", { input });
}

export function getSessionPage(
  codexDir?: string,
  options: { query?: string; limit?: number; rebuild?: boolean } = {},
) {
  if (!isTauri()) {
    const sessions = [
      {
        id: "019f7dcb-642d-70c2-a12d-19d7e603a8c0",
        title: "完善 Codex Companion 发布和用量能力",
        cwd: "/Users/demo/work/codex-companion",
        cwdAvailable: true,
        model: "gpt-5.6-codex",
        providerId: "official-team",
        path: `${MOCK_CODEX_DIR}/sessions/2026/07/22/rollout-demo.jsonl`,
        modifiedAt: new Date().toISOString(),
        bytes: 182_400,
        isSubagent: false,
        parentId: null,
        isRunning: true,
      },
    ];
    const query = options.query?.trim().toLowerCase() ?? "";
    const filtered = sessions.filter((session) => {
      if (!query) return true;
      return [session.id, session.title, session.cwd, session.model, session.providerId]
        .some((value) => value.toLowerCase().includes(query));
    });
    return Promise.resolve<SessionPage>({
      sessions: filtered.slice(0, options.limit ?? 50),
      total: filtered.length,
      query: options.query?.trim() ?? "",
      fromCache: !options.rebuild,
      dataRoot: codexDir?.trim() || MOCK_CODEX_DIR,
    });
  }
  return invoke<SessionPage>("get_session_page", {
    codexDir: emptyToNull(codexDir),
    query: emptyToNull(options.query),
    limit: options.limit ?? 50,
    rebuild: options.rebuild ?? false,
  });
}

export function getSessionProviderPreferences(sessionIds: string[]) {
  if (!isTauri()) {
    return Promise.resolve(Object.fromEntries(
      sessionIds.flatMap((sessionId) => {
        const providerId = mockSessionProviderPreferences[sessionId];
        return providerId ? [[sessionId, providerId]] : [];
      }),
    ));
  }
  return invoke<Record<string, string>>("get_session_provider_preferences", { sessionIds });
}

export function setSessionProviderPreference(sessionId: string, providerId: string) {
  if (!isTauri()) {
    mockSessionProviderPreferences = { ...mockSessionProviderPreferences, [sessionId]: providerId };
    return Promise.resolve(providerId);
  }
  return invoke<string>("set_session_provider_preference", { sessionId, providerId });
}

export function clearSessionProviderPreference(sessionId: string) {
  if (!isTauri()) {
    const { [sessionId]: _removed, ...remaining } = mockSessionProviderPreferences;
    mockSessionProviderPreferences = remaining;
    return Promise.resolve(Boolean(_removed));
  }
  return invoke<boolean>("clear_session_provider_preference", { sessionId });
}

export function createApiClient(input: ApiClientCreate) {
  if (!isTauri()) {
    const now = new Date().toISOString();
    const apiKey = `cc_live_mock_${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
    const client: ApiClient = {
      id: `client_${Date.now()}`,
      name: input.name.trim(),
      keyPrefix: apiKey.slice(0, 16),
      allowedModels: [...new Set(input.allowedModels.map((model) => model.trim()).filter(Boolean))].sort(),
      enabled: true,
      createdAt: now,
      lastUsedAt: null,
      requestCount: 0,
      usage: emptyApiClientUsage(),
      health: { status: "idle", lastRequestAt: null, lastSuccessAt: null, lastFailureAt: null, consecutiveFailures: 0 },
    };
    mockApiService = { ...mockApiService, clients: [client, ...mockApiService.clients] };
    return Promise.resolve<ApiClientSecret>({ client, apiKey });
  }
  return invoke<ApiClientSecret>("create_api_client", { input });
}

export function updateApiClient(input: ApiClientUpdate) {
  if (!isTauri()) {
    const existing = mockApiService.clients.find((client) => client.id === input.id);
    if (!existing) return Promise.reject(new Error(`unknown API client: ${input.id}`));
    const client: ApiClient = {
      ...existing,
      name: input.name.trim(),
      allowedModels: [...new Set(input.allowedModels.map((model) => model.trim()).filter(Boolean))].sort(),
      enabled: input.enabled,
    };
    mockApiService = {
      ...mockApiService,
      clients: mockApiService.clients.map((item) => (item.id === client.id ? client : item)),
    };
    return Promise.resolve(client);
  }
  return invoke<ApiClient>("update_api_client", { input });
}

export function rotateApiClientKey(id: string) {
  if (!isTauri()) {
    const client = mockApiService.clients.find((item) => item.id === id);
    if (!client) return Promise.reject(new Error(`unknown API client: ${id}`));
    const apiKey = `cc_live_mock_${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
    const rotated = { ...client, keyPrefix: apiKey.slice(0, 16) };
    mockApiService = {
      ...mockApiService,
      clients: mockApiService.clients.map((item) => (item.id === id ? rotated : item)),
    };
    return Promise.resolve<ApiClientSecret>({ client: rotated, apiKey });
  }
  return invoke<ApiClientSecret>("rotate_api_client_key", { id });
}

export function deleteApiClient(id: string) {
  if (!isTauri()) {
    const existed = mockApiService.clients.some((client) => client.id === id);
    mockApiService = {
      ...mockApiService,
      clients: mockApiService.clients.filter((client) => client.id !== id),
    };
    return Promise.resolve(existed);
  }
  return invoke<boolean>("delete_api_client", { id });
}

export function clearApiRequestLogs() {
  if (!isTauri()) {
    const count = mockApiService.recentRequests.length;
    mockApiService = { ...mockApiService, recentRequests: [] };
    return Promise.resolve(count);
  }
  return invoke<number>("clear_api_request_logs");
}

export function updateRelaySettings(input: RelaySettingsUpdate) {
  if (!isTauri()) {
    if (input.requireApiKey && !mockApiService.clients.some((client) => client.enabled)) {
      return Promise.reject(new Error("启用强制密钥前，至少需要一个已启用的 API client"));
    }
    const relay = { ...mockStatus.config.relay, ...input };
    mockStatus = {
      ...mockStatus,
      config: { ...mockStatus.config, relay },
    };
    return Promise.resolve<RelayConfig>(relay);
  }
  return invoke<RelayConfig>("update_relay_settings", { input });
}

export function apiServiceSelfTest() {
  if (!isTauri()) {
    return Promise.resolve<ApiServiceSelfTest>({
      ok: true,
      baseUrl: mockStatus.relayBaseUrl,
      latencyMs: 7,
      databaseOk: true,
      listenerOk: true,
      message: "配置数据库与本地 HTTP 监听均可用；未消耗上游账号额度",
    });
  }
  return invoke<ApiServiceSelfTest>("api_service_self_test");
}

export function install(codexDir?: string) {
  if (!isTauri()) {
    mockStatus = {
      ...mockStatus,
      codex: {
        ...mockStatus.codex,
        installed: true,
        message: "Codex 已配置为使用本地代理",
      },
    };
    return Promise.resolve(mockStatus.codex);
  }
  return invoke<CodexInstallStatus>("install", { codexDir: emptyToNull(codexDir) });
}

export function uninstall(codexDir?: string) {
  if (!isTauri()) {
    mockStatus = {
      ...mockStatus,
      codex: {
        ...mockStatus.codex,
        installed: false,
        message: "Codex 配置存在，但当前未使用 Companion",
      },
    };
    return Promise.resolve(mockStatus.codex);
  }
  return invoke<CodexInstallStatus>("uninstall", { codexDir: emptyToNull(codexDir) });
}

export function addProvider(input: ProviderUpsert) {
  if (!isTauri()) {
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers: {
          ...mockStatus.config.providers,
          [input.id]: input,
        },
        health: {
          ...mockStatus.config.health,
          [input.id]: {
            status: "unknown",
            lastChecked: null,
            lastSuccess: null,
            lastError: null,
            lastFailureKind: null,
            cooldownUntil: null,
            failureCount: 0,
          },
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(input);
  }
  return invoke<ProviderConfig>("add_provider", { input });
}

export function updateApiKeyProvider(input: ApiKeyProviderUpdate) {
  if (!isTauri()) {
    const existing = mockStatus.config.providers[input.id];
    if (!existing) return Promise.reject(new Error(`unknown provider: ${input.id}`));
    if (existing.kind === "official_codex") {
      return Promise.reject(new Error("官方 Codex 账号不能按 API Key provider 编辑"));
    }
    const authRef =
      input.apiKey && input.apiKey.trim()
        ? `file:${mockStatus.dataDir}/auth/api-keys/${input.id}.json`
        : input.envVar && input.envVar.trim()
          ? `env:${input.envVar.trim()}`
          : existing.authRef;
    const directAuthRef =
      input.apiKey && input.apiKey.trim()
        ? null
        : input.envVar && input.envVar.trim()
          ? `env:${input.envVar.trim()}`
          : existing.directAuthRef;
    let usageQuery = existing.account?.usageQuery ?? null;
    if (input.usageQuery) {
      usageQuery = input.usageQuery.enabled
        ? {
            template: input.usageQuery.template,
            baseUrl: input.usageQuery.baseUrl?.trim() || input.baseUrl.trim(),
            script: input.usageQuery.script?.trim() || "",
            timeoutSeconds: input.usageQuery.timeoutSeconds ?? 10,
          }
        : null;
    }
    const provider: ProviderConfig = {
      ...existing,
      name: input.providerName.trim(),
      kind: input.kind,
      baseUrl: input.baseUrl.trim().replace(/\/+$/, ""),
      websocketUrl: input.websocketUrl?.trim() || null,
      authRef,
      directAuthRef,
      refreshIntervalSeconds: input.refreshIntervalSeconds || existing.refreshIntervalSeconds,
      account: {
        ...(existing.account ?? {}),
        email: input.providerDisplayName?.trim() || existing.account?.email || null,
        displayName: input.providerName.trim(),
        subscriptionType: "API Key",
        usageQuery,
      },
    };
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers: {
          ...mockStatus.config.providers,
          [input.id]: provider,
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(provider);
  }
  return invoke<ProviderConfig>("update_api_key_provider", { input });
}

export function exportProviderJson(id: string, format?: ProviderExportFormat | null) {
  if (!isTauri()) {
    const provider = mockStatus.config.providers[id];
    if (!provider) return Promise.reject(new Error(`unknown provider: ${id}`));
    const fileNameBase = `${sanitizeProviderId(provider.name)}${format && format !== "codex_companion" ? `_${format}` : ""}`;
    if (provider.kind !== "official_codex") {
      return Promise.resolve<ProviderExportOutput>({
        fileNameBase,
        jsonContent: JSON.stringify(
          [
            {
              auth_mode: "apikey",
              OPENAI_API_KEY: "sk-mock-api-key",
              email: provider.account?.email || provider.account?.displayName || provider.name,
              api_base_url: provider.baseUrl,
              api_provider_id: provider.id,
              api_provider_name: provider.name,
            },
          ],
          null,
          2,
        ),
      });
    }
    const cpa = {
      id_token: "mock-id-token",
      access_token: "mock-access-token",
      refresh_token: "mock-refresh-token",
      account_id: provider.account?.accountId || provider.id,
      last_refresh: new Date().toISOString(),
      email: provider.account?.email || provider.name,
      type: "codex",
      expired: provider.account?.validUntil || "",
    };
    if (format === "sub2api") {
      return Promise.resolve<ProviderExportOutput>({
        fileNameBase,
        jsonContent: JSON.stringify(
          {
            exported_at: new Date().toISOString(),
            proxies: [],
            accounts: [
              {
                name: provider.account?.displayName || provider.name,
                platform: "openai",
                type: "oauth",
                credentials: {
                  access_token: cpa.access_token,
                  refresh_token: cpa.refresh_token,
                  id_token: cpa.id_token,
                  email: cpa.email,
                  chatgpt_account_id: cpa.account_id,
                  plan_type: provider.account?.subscriptionType,
                },
                concurrency: 0,
                priority: 0,
              },
            ],
            type: "sub2api-data",
            version: 1,
          },
          null,
          2,
        ),
      });
    }
    return Promise.resolve<ProviderExportOutput>({
      fileNameBase,
      jsonContent: JSON.stringify(cpa, null, 2),
    });
  }
  return invoke<ProviderExportOutput>("export_provider_json", { id, format: format ?? null });
}

export function importApiKeyProvider(input: {
  providerName: string;
  kind: "openai_compatible" | "relay_provider";
  baseUrl: string;
  websocketUrl?: string;
  apiKey: string;
  envVar?: string;
  model?: string;
  refreshIntervalSeconds?: number;
  usageQueryEnabled?: boolean;
  usageQueryTemplate?: ProviderUsageQueryTemplate;
  usageQueryScript?: string;
  usageQueryTimeoutSeconds?: number;
  usageQueryApiKey?: string;
  usageQueryBaseUrl?: string;
  usageQueryAccessToken?: string;
  usageQueryUserId?: string;
}) {
  if (!isTauri()) {
    const id = `${sanitizeProviderId(input.providerName)}_${accountIdHash(input.baseUrl)}`;
    const provider: ProviderConfig = {
      id,
      name: input.providerName,
      kind: input.kind,
      baseUrl: input.baseUrl.replace(/\/+$/, ""),
      websocketUrl: input.websocketUrl?.trim() || null,
      authRef: input.apiKey ? `file:${mockStatus.dataDir}/auth/api-keys/${id}.json` : input.envVar ? `env:${input.envVar}` : null,
      directAuthRef: input.envVar ? `env:${input.envVar}` : null,
      modelMap: input.model ? { [input.model]: input.model } : {},
      priority: 100,
      enabled: true,
      refreshIntervalSeconds: input.refreshIntervalSeconds || 60,
      account: {
        displayName: input.providerName,
        subscriptionType: "API Key",
        usageQuery: input.usageQueryEnabled
          ? {
              template: input.usageQueryTemplate ?? "general",
              baseUrl: input.usageQueryBaseUrl?.trim() || input.baseUrl.trim(),
              script: input.usageQueryScript?.trim() || "",
              timeoutSeconds: input.usageQueryTimeoutSeconds ?? 10,
            }
          : null,
      },
    };
    const created = !mockStatus.config.providers[id];
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers: {
          ...mockStatus.config.providers,
          [id]: provider,
        },
        health: {
          ...mockStatus.config.health,
          [id]: {
            status: "unknown",
            lastChecked: null,
            lastSuccess: null,
            lastError: null,
            lastFailureKind: null,
            cooldownUntil: null,
            failureCount: 0,
          },
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve<ProviderImportOutcome>({
      provider,
      importKind: "api_key",
      accountId: "api_key",
      authPath: `${mockStatus.dataDir}/auth/api-keys/${id}.json`,
      created,
      message: created ? "已导入 API Key provider" : "已更新 API Key provider",
    });
  }
  return invoke<ProviderImportOutcome>("import_api_key_provider", {
    input: {
      providerName: input.providerName,
      kind: input.kind,
      baseUrl: input.baseUrl,
      websocketUrl: emptyToNull(input.websocketUrl),
      apiKey: input.apiKey,
      envVar: emptyToNull(input.envVar),
      model: emptyToNull(input.model),
      refreshIntervalSeconds: input.refreshIntervalSeconds ?? null,
      usageQuery: {
        enabled: Boolean(input.usageQueryEnabled),
        template: input.usageQueryTemplate ?? "general",
        baseUrl: emptyToNull(input.usageQueryBaseUrl),
        script: emptyToNull(input.usageQueryScript),
        timeoutSeconds: input.usageQueryTimeoutSeconds ?? 10,
        apiKey: emptyToNull(input.usageQueryApiKey),
        accessToken: emptyToNull(input.usageQueryAccessToken),
        userId: emptyToNull(input.usageQueryUserId),
      },
    },
  });
}

export function testUsageQuery(input: ProviderUsageQueryTestInput) {
  if (!isTauri()) {
    const total = input.usageQuery.template === "new_api" ? 1120.5 : 100;
    const used = input.usageQuery.template === "new_api" ? 856.2 : 24.7;
    return Promise.resolve<ProviderAccountInfo>({
      displayName: "查询测试",
      subscriptionType: input.usageQuery.template === "new_api" ? "default" : "API Key",
      subscriptionStatus: "可用",
      quotaLabel: "USD 余额",
      quotaPercent: ((total - used) / total) * 100,
      usageTotal: total,
      usageUsed: used,
      usageAvailable: total - used,
      lastRefreshAt: new Date().toISOString(),
    });
  }
  return invoke<ProviderAccountInfo>("test_usage_query", { input });
}

export function addEnvProvider(input: {
  providerName: string;
  kind: Extract<ProviderKind, "openai_compatible" | "relay_provider">;
  baseUrl: string;
  envVar: string;
  model?: string;
  refreshIntervalSeconds?: number;
}) {
  const providerName = input.providerName.trim();
  const baseUrl = input.baseUrl.trim().replace(/\/+$/, "");
  const envVar = input.envVar.trim();
  const model = input.model?.trim();
  const id = `${sanitizeProviderId(providerName)}_${accountIdHash(`${baseUrl}:${envVar}`)}`;
  return addProvider({
    id,
    name: providerName,
    kind: input.kind,
    baseUrl,
    authRef: `env:${envVar}`,
    directAuthRef: `env:${envVar}`,
    modelMap: model ? { [model]: model } : {},
    priority: 100,
    enabled: true,
    refreshIntervalSeconds: input.refreshIntervalSeconds || 60,
    account: {
      displayName: providerName,
      subscriptionType: "Env API Key",
    },
  });
}

export function importProviderJson(jsonText: string, providerId?: string, providerName?: string) {
  if (!isTauri()) {
    const value = JSON.parse(jsonText) as unknown;
    const credentialShape = mockProviderCredentialShape(value);
    if (credentialShape.hasAgentIdentity && (!credentialShape.runtimeId || !credentialShape.privateKey)) {
      return Promise.reject(new Error("Agent Identity 缺少 runtime id 或私钥"));
    }
    if (!credentialShape.hasAgentIdentity && credentialShape.hasApiKeyShape) {
      return importApiKeyJsonMock(value, providerId, providerName);
    }
    if (!credentialShape.hasAgentIdentity && !credentialShape.officialToken) {
      return Promise.reject(new Error("仅支持 Codex OAuth、Agent Identity 或 API Key 账号 JSON"));
    }
    const explicitName = emptyToNull(providerName);
    const detectedName = findString(value, ["email", "name"]);
    let importedName = explicitName;
    if (!importedName && detectedName && !isGenericOfficialAccountName(detectedName)) {
      importedName = detectedName;
    }
    const detectedAccountId = findString(value, ["chatgpt_account_id", "account_id", "workspace_id"]);
    if (credentialShape.hasAgentIdentity && !detectedAccountId) {
      return Promise.reject(new Error("Agent Identity 缺少 ChatGPT account id"));
    }
    const accountId = detectedAccountId || findString(value, ["email"]) || `mock_${Date.now()}`;
    const name = importedName || accountId;
    const userId = findString(value, ["chatgpt_user_id", "user_id", "userId", "user"]);
    const email = findString(value, ["email", "name"]);
    const teamName = findString(value, ["team_name", "teamName", "workspace_name", "workspaceName"]);
    const subscriptionType = findString(value, [
      "subscription_type",
      "subscriptionType",
      "plan_type",
      "planType",
      "auth_file_plan_type",
      "authFilePlanType",
      "chatgpt_plan_type",
    ]);
    const validUntil = findString(value, [
      "valid_until",
      "validUntil",
      "expires_at",
      "expiresAt",
      "expired",
      "subscription_expires_at",
      "subscriptionExpiresAt",
      "subscription_active_until",
      "subscriptionActiveUntil",
      "chatgpt_subscription_active_until",
      "active_until",
      "activeUntil",
    ]);
    const quotaResetAt = findString(value, ["quota_reset_at", "quotaResetAt", "reset_at", "resetAt"]);
    const quotaWindows = extractQuotaWindows(value);
    const id = normalizeMockProviderId(providerId) || `codex_openai_${sanitizeProviderId(name)}_${accountIdHash(accountId)}`;
    const provider: ProviderConfig = {
      id,
      name,
      kind: "official_codex",
      baseUrl: "https://chatgpt.com/backend-api/codex",
      authRef: `file:${mockStatus.dataDir}/auth/accounts/${id}.json`,
      modelMap: {},
      priority: 50,
      enabled: true,
      refreshIntervalSeconds: 60,
      account: {
        authMode: mockOfficialAuthMode(credentialShape),
        displayName: name,
        email,
        teamName,
        accountId,
        userId,
        subscriptionType: subscriptionType?.toUpperCase() || null,
        quotaLabel: findString(value, ["quota_label", "quotaLabel", "usage_label", "usageLabel"]),
        quotaPercent:
          findNumber(value, ["quota_percent", "quotaPercent", "usage_percent", "usagePercent", "percent"]) ??
          lowestQuotaPercent(quotaWindows),
        quotaResetAt,
        quotaWindows,
        validUntil,
        lastRefreshAt: findString(value, ["last_refresh", "lastRefresh", "last_refresh_at", "lastRefreshAt"]),
      },
    };
    const created = !mockStatus.config.providers[id];
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers: {
          ...mockStatus.config.providers,
          [id]: provider,
        },
        health: {
          ...mockStatus.config.health,
          [id]: {
            status: "unknown",
            lastChecked: null,
            lastSuccess: null,
            lastError: null,
            lastFailureKind: null,
            cooldownUntil: null,
            failureCount: 0,
          },
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    const importKind = credentialShape.hasAgentIdentity ? "agent_identity" : "openai_account";
    let message = created ? "已导入 Codex 官方账号 provider" : "已更新 Codex 官方账号 provider";
    if (credentialShape.hasAgentIdentity) {
      message = created ? "已导入 Agent Identity provider" : "已更新 Agent Identity provider";
    }
    return Promise.resolve<ProviderImportOutcome>({
      provider,
      importKind,
      accountId,
      authPath: `${mockStatus.dataDir}/auth/accounts/${id}.json`,
      created,
      message,
    });
  }
  return invoke<ProviderImportOutcome>("import_provider_json", {
    jsonText,
    providerId: emptyToNull(providerId),
    providerName: emptyToNull(providerName),
  });
}

export async function importProviderJsonMany(
  jsonText: string,
  providerId?: string,
  providerName?: string,
  addToGroupId?: string | null,
): Promise<ProviderImportBatchReport> {
  if (!isTauri()) {
    const value = JSON.parse(jsonText) as unknown;
    const items = expandProviderJsonItems(value, providerId);
    const report: ProviderImportBatchReport = {
      total: items.length,
      succeeded: [],
      failed: [],
      addedToGroup: [],
    };
    for (const [index, item] of items.entries()) {
      try {
        const outcome = await importProviderJson(
          JSON.stringify(item),
          items.length === 1 ? providerId : undefined,
          providerName,
        );
        report.succeeded.push(outcome);
      } catch (unknownError) {
        report.failed.push({
          index,
          label: `账号 ${index + 1}`,
          message: userFacingError(unknownError),
        });
      }
    }
    if (addToGroupId) {
      const group = mockStatus.config.groups[addToGroupId];
      if (group) {
        const providerIds = report.succeeded.map((outcome) => outcome.provider.id);
        report.addedToGroup = providerIds.filter((id) => !group.providerOrder.includes(id));
        group.providerOrder = [...group.providerOrder, ...report.addedToGroup];
      }
    }
    return report;
  }
  return invoke<ProviderImportBatchReport>("import_provider_json_many", {
    jsonText,
    providerId: emptyToNull(providerId),
    providerName: emptyToNull(providerName),
    addToGroupId: emptyToNull(addToGroupId ?? undefined),
  });
}

export async function reviewProviderJsonMany(
  jsonText: string,
  providerId?: string,
  providerName?: string,
): Promise<ProviderImportReviewReport> {
  if (!isTauri()) {
    const value = JSON.parse(jsonText) as unknown;
    const items = expandProviderJsonItems(value, providerId);
    const report: ProviderImportReviewReport = {
      total: items.length,
      ready: [],
      failed: [],
    };
    const reviewedProviderIds = new Set<string>();
    for (const [index, item] of items.entries()) {
      try {
        const reviewedItem = reviewProviderJsonMock(item, index, providerId, providerName);
        reviewedItem.willOverwrite ||= reviewedProviderIds.has(reviewedItem.providerId);
        reviewedProviderIds.add(reviewedItem.providerId);
        report.ready.push(reviewedItem);
      } catch (unknownError) {
        report.failed.push({
          index,
          label: providerImportItemLabel(item, index),
          message: userFacingError(unknownError),
        });
      }
    }
    return report;
  }
  return invoke<ProviderImportReviewReport>("review_provider_json_many", {
    jsonText,
    providerId: emptyToNull(providerId),
    providerName: emptyToNull(providerName),
  });
}

export function importLocalCodexProvider(codexDir?: string) {
  if (!isTauri()) {
    return importProviderJson(
      JSON.stringify({
        tokens: {
          access_token: "local-access-token",
          id_token: "local-id-token",
          refresh_token: "local-refresh-token",
          email: "local-codex@example.com",
          chatgpt_account_id: "local-codex-account",
          plan_type: "team",
        },
        extra: {
          team_name: "mock-team",
          user_id: "mock-user-id",
          hourly_percentage: 82,
          hourly_reset_time: new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString(),
          weekly_percentage: 77,
          weekly_reset_time: new Date(Date.now() + 30 * 60 * 60 * 1000).toISOString(),
          subscription_active_until: new Date(Date.now() + 29 * 24 * 60 * 60 * 1000).toISOString(),
        },
      }),
    );
  }
  return invoke<ProviderImportOutcome>("import_local_codex_provider", {
    codexDir: emptyToNull(codexDir),
  });
}

export function startCodexOAuth() {
  if (!isTauri()) {
    const now = Math.floor(Date.now() / 1000);
    if (mockOAuthPending && mockOAuthPending.expiresAt > now) {
      return Promise.resolve<CodexOAuthStartResponse>(structuredClone(mockOAuthPending));
    }
    const loginId = `mock-login-${Date.now()}`;
    const callbackUrl = "http://localhost:1455/auth/callback";
    mockOAuthPending = {
      loginId,
      authUrl: `https://auth.openai.com/oauth/authorize?response_type=code&state=${encodeURIComponent(loginId)}`,
      callbackUrl,
      expiresAt: now + 300,
      callbackServerReady: true,
      callbackReceived: false,
    };
    return Promise.resolve<CodexOAuthStartResponse>(structuredClone(mockOAuthPending));
  }
  return invoke<CodexOAuthStartResponse>("start_codex_oauth");
}

export function getCodexOAuthStatus() {
  if (!isTauri()) {
    if (!mockOAuthPending || mockOAuthPending.expiresAt <= Math.floor(Date.now() / 1000)) {
      mockOAuthPending = null;
      return Promise.resolve<CodexOAuthStatus | null>(null);
    }
    return Promise.resolve<CodexOAuthStatus>({
      loginId: mockOAuthPending.loginId,
      authUrl: mockOAuthPending.authUrl,
      callbackUrl: mockOAuthPending.callbackUrl,
      expiresAt: mockOAuthPending.expiresAt,
      callbackReceived: mockOAuthPending.callbackReceived,
      callbackServerReady: mockOAuthPending.callbackServerReady,
      error: null,
    });
  }
  return invoke<CodexOAuthStatus | null>("get_codex_oauth_status");
}

export function openCodexOAuth(loginId: string) {
  if (!isTauri()) {
    if (!mockOAuthPending || mockOAuthPending.loginId !== loginId) {
      return Promise.reject(new Error("OAuth 授权会话已失效，请重新开始"));
    }
    window.setTimeout(() => {
      if (mockOAuthPending?.loginId === loginId) {
        mockOAuthPending.callbackReceived = true;
      }
    }, 800);
    return Promise.resolve();
  }
  return invoke<void>("open_codex_oauth", { loginId });
}

export function submitCodexOAuthCallback(loginId: string, callbackUrl: string) {
  if (!isTauri()) {
    if (!mockOAuthPending || mockOAuthPending.loginId !== loginId) {
      return Promise.reject(new Error("OAuth 授权会话已失效，请重新开始"));
    }
    if (!callbackUrl.includes("code=") || !callbackUrl.includes("state=")) {
      return Promise.reject(new Error("回调地址缺少 code 或 state 参数"));
    }
    mockOAuthPending.callbackReceived = true;
    return Promise.resolve();
  }
  return invoke<void>("submit_codex_oauth_callback", { loginId, callbackUrl });
}

export function cancelCodexOAuth(loginId?: string) {
  if (!isTauri()) {
    if (!loginId || mockOAuthPending?.loginId === loginId) {
      mockOAuthPending = null;
    }
    return Promise.resolve();
  }
  return invoke<void>("cancel_codex_oauth", { loginId: emptyToNull(loginId) });
}

export async function completeCodexOAuth(loginId: string): Promise<ProviderImportOutcome> {
  if (!isTauri()) {
    if (!mockOAuthPending || mockOAuthPending.loginId !== loginId || !mockOAuthPending.callbackReceived) {
      throw new Error("尚未收到 OAuth 回调，请先完成浏览器授权");
    }
    const providerName = "oauth-user@example.com";
    const accountId = mockOAuthPending.loginId;
    mockOAuthPending = null;
    return importProviderJson(
      JSON.stringify({
        tokens: {
          access_token: "mock-oauth-access-token",
          id_token: "mock-oauth-id-token",
          refresh_token: "mock-oauth-refresh-token",
          email: providerName,
          chatgpt_account_id: accountId,
        },
      }),
      undefined,
      providerName,
    );
  }
  return invoke<ProviderImportOutcome>("complete_codex_oauth", { loginId });
}

export function removeProvider(id: string) {
  if (!isTauri()) {
    const { [id]: _provider, ...providers } = mockStatus.config.providers;
    const { [id]: _health, ...health } = mockStatus.config.health;
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers,
        health,
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(Boolean(_provider));
  }
  return invoke<boolean>("remove_provider", { id });
}

export function testProvider(id: string) {
  if (!isTauri()) {
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        health: {
          ...mockStatus.config.health,
          [id]: {
            status: "healthy",
            lastChecked: new Date().toISOString(),
            lastSuccess: new Date().toISOString(),
            lastError: null,
            lastFailureKind: null,
            cooldownUntil: null,
            failureCount: 0,
          },
        },
      },
    };
    return Promise.resolve();
  }
  return invoke<void>("test_provider", { id });
}

export function refreshProvider(id: string) {
  if (!isTauri()) {
    const health: ProviderHealth = {
      status: "healthy",
      lastChecked: new Date().toISOString(),
      lastSuccess: new Date().toISOString(),
      lastError: null,
      lastFailureKind: null,
      cooldownUntil: null,
      failureCount: 0,
    };
    const provider = mockStatus.config.providers[id];
    if (!provider) {
      return Promise.reject(new Error(`unknown provider: ${id}`));
    }
    const account =
      provider?.account && provider.kind === "official_codex"
        ? {
            ...provider.account,
            subscriptionStatus: "可用",
            quotaWindows: [
              {
                label: "5h",
                remainingPercent: 82,
                resetAt: new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString(),
                windowMinutes: 300,
              },
              {
                label: "Week",
                remainingPercent: 77,
                resetAt: new Date(Date.now() + 30 * 60 * 60 * 1000).toISOString(),
                windowMinutes: 10_080,
              },
            ],
            quotaLabel: "5h 82% / Week 77%",
            quotaPercent: 77,
            validUntil: provider.account.validUntil ?? new Date(Date.now() + 29 * 24 * 60 * 60 * 1000).toISOString(),
            lastRefreshAt: new Date().toISOString(),
          }
        : provider?.account
          ? {
              ...provider.account,
              subscriptionStatus: "可用",
              quotaLabel: "750 / 1000",
              quotaPercent: 75,
              quotaWindows: [
                {
                  label: "API",
                  remainingPercent: 75,
                  resetAt: new Date(Date.now() + 14 * 24 * 60 * 60 * 1000).toISOString(),
                  windowMinutes: null,
                },
              ],
            usageTotal: 1000,
            usageUsed: 250,
            usageAvailable: 750,
            validUntil: provider.account.validUntil ?? new Date(Date.now() + 14 * 24 * 60 * 60 * 1000).toISOString(),
            lastRefreshAt: new Date().toISOString(),
          }
          : provider?.account;
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        providers: {
          ...mockStatus.config.providers,
          [id]: {
            ...provider,
            account,
          },
        },
        health: {
          ...mockStatus.config.health,
          [id]: health,
        },
      },
    };
    return Promise.resolve(health);
  }
  return invoke<ProviderHealth>("refresh_provider", { id });
}

export function refreshAllProviders() {
  if (!isTauri()) {
    const health = Object.keys(mockStatus.config.providers).map((id) => {
      const next: ProviderHealth = {
        status: "healthy",
        lastChecked: new Date().toISOString(),
        lastSuccess: new Date().toISOString(),
        lastError: null,
        lastFailureKind: null,
        cooldownUntil: null,
        failureCount: 0,
      };
      const provider = mockStatus.config.providers[id];
      if (provider?.account) {
        mockStatus.config.providers[id] = {
          ...provider,
          account: {
            ...provider.account,
            subscriptionStatus: "可用",
            quotaWindows:
              provider.kind === "official_codex"
                ? [
                    {
                      label: "5h",
                      remainingPercent: 82,
                      resetAt: new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString(),
                      windowMinutes: 300,
                    },
                    {
                      label: "Week",
                      remainingPercent: 77,
                      resetAt: new Date(Date.now() + 30 * 60 * 60 * 1000).toISOString(),
                      windowMinutes: 10_080,
                    },
                  ]
                : [
                    {
                      label: "API",
                      remainingPercent: 75,
                      resetAt: new Date(Date.now() + 14 * 24 * 60 * 60 * 1000).toISOString(),
                      windowMinutes: null,
                    },
                  ],
            quotaLabel: provider.kind === "official_codex" ? "5h 82% / Week 77%" : "750 / 1000",
            quotaPercent: provider.kind === "official_codex" ? 77 : 75,
            usageTotal: provider.kind === "official_codex" ? provider.account.usageTotal : 1000,
            usageUsed: provider.kind === "official_codex" ? provider.account.usageUsed : 250,
            usageAvailable: provider.kind === "official_codex" ? provider.account.usageAvailable : 750,
            validUntil:
              provider.account.validUntil ??
              new Date(Date.now() + (provider.kind === "official_codex" ? 29 : 14) * 24 * 60 * 60 * 1000).toISOString(),
            lastRefreshAt: new Date().toISOString(),
          },
        };
      }
      mockStatus.config.health[id] = next;
      return next;
    });
    mockStatus = { ...mockStatus, config: { ...mockStatus.config, health: { ...mockStatus.config.health } } };
    return Promise.resolve(health);
  }
  return invoke<ProviderHealth[]>("refresh_all_providers");
}

export function upsertGroup(input: GroupUpsert) {
  if (!isTauri()) {
    const previousGroup = mockStatus.config.groups[input.id];
    const group: ProviderGroup = {
      ...input,
      priorityFailbackRevision: previousGroup?.priorityFailbackRevision ?? 0,
      priorityFailbackTargetProviderId: previousGroup?.priorityFailbackTargetProviderId ?? null,
    };
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        groups: {
          ...mockStatus.config.groups,
          [input.id]: group,
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(group);
  }
  return invoke<ProviderGroup>("upsert_group", { input });
}

export function requestPriorityFailback(id: string, providerId: string) {
  if (!isTauri()) {
    const group = mockStatus.config.groups[id];
    if (!group) return Promise.reject(new Error(`unknown group: ${id}`));
    const updatedGroup = {
      ...group,
      priorityFailbackRevision: group.priorityFailbackRevision + 1,
      priorityFailbackTargetProviderId: providerId,
    };
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        groups: {
          ...mockStatus.config.groups,
          [id]: updatedGroup,
        },
      },
    };
    return Promise.resolve(updatedGroup);
  }
  return invoke<ProviderGroup>("request_priority_failback", { id, providerId });
}

export function useGroup(id: string) {
  if (!isTauri()) {
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        relay: {
          ...mockStatus.config.relay,
          activeGroupId: id,
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(mockStatus.config.groups[id]);
  }
  return invoke<ProviderGroup>("use_group", { id });
}

export function launchGroup(id: string, codexDir?: string) {
  if (!isTauri()) {
    const group = mockStatus.config.groups[id];
    if (!group) return Promise.reject(new Error(`unknown group: ${id}`));
    const restartRequired = mockRelayRestartRequired();
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        relay: {
          ...mockStatus.config.relay,
          activeGroupId: id,
        },
      },
      codex: {
        ...mockStatus.codex,
        installed: true,
        modelProvider: "codex-companion",
        message: "Codex 已配置为使用本地代理",
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    recordMockCodexLaunch("group_relay", "codex-companion");
    return Promise.resolve(mockLaunchOutcome("group_relay", id, "codex-companion", codexDir, restartRequired));
  }
  return invoke<CodexLaunchOutcome>("launch_group", { id, codexDir: emptyToNull(codexDir) });
}

export function launchProvider(id: string, mode: ProviderLaunchMode = "auto", codexDir?: string) {
  if (!isTauri()) {
    const provider = mockStatus.config.providers[id];
    if (!provider) return Promise.reject(new Error(`unknown provider: ${id}`));
    const shouldDirect = providerCanDirectConnect(provider) && (mode === "direct" || mode === "auto");
    if (mode === "direct" && !providerCanDirectConnect(provider)) {
      return Promise.reject(
        new Error(
          provider.kind === "official_codex"
            ? "官方 Codex 账号必须使用 Companion 本地代理，才能独立刷新 OAuth token 并保活"
            : `${provider.name} 缺少直连所需的账号材料、API Key 文件或环境变量`,
        ),
      );
    }
    if (shouldDirect) {
      const targetProviderId = provider.kind === "official_codex" ? "openai" : provider.id;
      mockStatus = {
        ...mockStatus,
        codex: {
          ...mockStatus.codex,
          installed: true,
          modelProvider: targetProviderId,
          companionBaseUrl: provider.baseUrl,
          message: `Codex 已配置为直连: ${provider.name}`,
        },
      };
      recordMockCodexLaunch("provider_direct", provider.id);
      return Promise.resolve(mockLaunchOutcome("provider_direct", id, targetProviderId, codexDir, true));
    }
    const restartRequired = mockRelayRestartRequired();
    const groupId = `single-${provider.id}`;
    const group: ProviderGroup = {
      id: groupId,
      name: `${provider.name} 单 Provider`,
      policy: "manual",
      providerOrder: [provider.id],
      providerWeights: {},
      fallbackEnabled: false,
      priorityFailbackIntervalSeconds: 0,
      priorityFailbackRevision: 0,
      priorityFailbackTargetProviderId: null,
    };
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        groups: {
          ...mockStatus.config.groups,
          [groupId]: group,
        },
        relay: {
          ...mockStatus.config.relay,
          activeGroupId: groupId,
        },
      },
      codex: {
        ...mockStatus.codex,
        installed: true,
        modelProvider: "codex-companion",
        message: "Codex 已配置为使用本地代理",
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    recordMockCodexLaunch("provider_relay", "codex-companion");
    return Promise.resolve(mockLaunchOutcome("provider_relay", id, "codex-companion", codexDir, restartRequired));
  }
  return invoke<CodexLaunchOutcome>("launch_provider", { id, codexDir: emptyToNull(codexDir), mode });
}

export function setProviderLaunchMode(providerId: string, mode: ProviderLaunchMode) {
  if (!isTauri()) {
    const provider = mockStatus.config.providers[providerId];
    if (provider && mode === "direct" && !providerCanDirectConnect(provider)) {
      const message = provider.kind === "official_codex"
        ? "官方 Codex 账号必须使用 Companion 本地代理，不能保存为直连模式"
        : `${provider.name} 当前不满足直连条件，不能保存为直连模式`;
      return Promise.reject(new Error(message));
    }
    const previousMode =
      mockStatus.config.app.providerLaunchModes[providerId] ?? "direct";
    const needsRelayRestart = previousMode === "direct" && mode === "relay";
    const providerLaunchModes = {
      ...mockStatus.config.app.providerLaunchModes,
      [providerId]: mode,
    };
    if (mode === "auto") {
      delete providerLaunchModes[providerId];
    }
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        app: {
          ...mockStatus.config.app,
          providerLaunchModes,
          codexRestartRequiredOnNextRelay:
            mockStatus.config.app.codexRestartRequiredOnNextRelay || needsRelayRestart,
        },
      },
    };
    writeStoredAppSettings(mockStatus.config.app);
    return Promise.resolve(mode);
  }
  return invoke<ProviderLaunchMode>("set_provider_launch_mode", { providerId, mode });
}

export function repair(
  history: boolean,
  plugins: boolean,
  dryRun: boolean,
  codexDir?: string,
  targetProviderId?: string,
) {
  const resolvedTargetProviderId = targetProviderId || mockStatus.codex.modelProvider || "codex-companion";
  if (!isTauri()) {
    return Promise.resolve({
      plan: {
        codexDir: codexDir || mockStatus.codex.codexDir,
        targetProviderId: resolvedTargetProviderId,
        historyFiles: history ? 2 : 0,
        historyLines: history ? 2 : 0,
        pluginFiles: plugins ? 1 : 0,
        stateRows: history ? 3 : 0,
        sourceProviderIds: ["openai", "custom"],
        dryRun,
      },
      backupRoot: dryRun ? null : `${mockStatus.codex.codexDir}/backups/codex-companion/mock`,
      migratedHistoryFiles: dryRun ? 0 : 2,
      migratedHistoryLines: dryRun ? 0 : 2,
      migratedPluginFiles: dryRun ? 0 : 1,
      migratedStateRows: dryRun ? 0 : 3,
      skippedReason: null,
    });
  }
  return invoke<RepairOutcome>("repair", {
    history,
    plugins,
    dryRun,
    codexDir: emptyToNull(codexDir),
    targetProviderId: emptyToNull(targetProviderId),
  });
}

export function setTheme(theme: ThemeMode) {
  if (!isTauri()) {
    writeStoredAppSettings({ ...mockStatus.config.app, theme });
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        app: { ...mockStatus.config.app, theme },
      },
    };
    return Promise.resolve(theme);
  }
  return invoke<ThemeMode>("set_theme", { theme });
}

export function setProviderViewMode(mode: ProviderViewMode) {
  if (!isTauri()) {
    writeStoredAppSettings({ ...mockStatus.config.app, providerViewMode: mode });
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        app: { ...mockStatus.config.app, providerViewMode: mode },
      },
    };
    return Promise.resolve(mode);
  }
  return invoke<ProviderViewMode>("set_provider_view_mode", { mode });
}

export function setPreserveOfficialCodexAuth(preserve: boolean) {
  if (!isTauri()) {
    writeStoredAppSettings({ ...mockStatus.config.app, preserveOfficialCodexAuth: preserve });
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        app: { ...mockStatus.config.app, preserveOfficialCodexAuth: preserve },
      },
    };
    return Promise.resolve(preserve);
  }
  return invoke<boolean>("set_preserve_official_codex_auth", { preserve });
}

export function setTokenUsageRefreshInterval(seconds: number) {
  if (!isTauri()) {
    const app = { ...mockStatus.config.app, tokenUsageRefreshIntervalSeconds: seconds };
    writeStoredAppSettings(app);
    mockStatus = {
      ...mockStatus,
      config: { ...mockStatus.config, app },
    };
    return Promise.resolve(seconds);
  }
  return invoke<number>("set_token_usage_refresh_interval", { seconds });
}

export function resetAppSettings() {
  if (!isTauri()) {
    const app = {
      ...defaultAppSettings(),
      preserveOfficialCodexAuth: mockStatus.config.app.preserveOfficialCodexAuth,
    };
    writeStoredAppSettings(app);
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        app,
      },
    };
    return Promise.resolve(app);
  }
  return invoke<AppSettings>("reset_app_settings");
}

export function getTokenUsage(codexDir?: string, query?: TokenUsageQuery) {
  return getTokenUsageFromRuntime(codexDir, query);
}

function emptyToNull(value?: string) {
  return value && value.trim() ? value.trim() : null;
}

function expandProviderJsonItems(value: unknown, providerId?: string): unknown[] {
  if (Array.isArray(value) && value.length > 0) {
    if (emptyToNull(providerId)) {
      throw new Error("批量 provider JSON 不能同时指定单个 provider id");
    }
    return value;
  }
  const accounts =
    value && typeof value === "object" && !Array.isArray(value)
      ? (value as { accounts?: unknown }).accounts
      : null;
  if (!Array.isArray(accounts) || accounts.length <= 1) {
    return [value];
  }
  if (emptyToNull(providerId)) {
    throw new Error("批量账号 JSON 不能同时指定单个 provider id");
  }
  return accounts.map((account) => ({
    ...(value as Record<string, unknown>),
    accounts: [account],
  }));
}

function reviewProviderJsonMock(
  value: unknown,
  index: number,
  providerId?: string,
  providerName?: string,
): ProviderImportReviewItem {
  const credentialShape = mockProviderCredentialShape(value);
  if (credentialShape.hasAgentIdentity && (!credentialShape.runtimeId || !credentialShape.privateKey)) {
    throw new Error("Agent Identity 缺少 runtime id 或私钥");
  }
  if (!credentialShape.hasAgentIdentity && credentialShape.hasApiKeyShape) {
    return reviewApiKeyJsonMock(value, index, providerId, providerName);
  }
  if (!credentialShape.hasAgentIdentity && !credentialShape.officialToken) {
    throw new Error("仅支持 Codex OAuth、Agent Identity 或 API Key 账号 JSON");
  }
  const accountId = findString(value, ["chatgpt_account_id", "account_id", "accountId", "workspace_id"]);
  if (credentialShape.hasAgentIdentity && !accountId) {
    throw new Error("Agent Identity 缺少 ChatGPT account id");
  }
  const name =
    emptyToNull(providerName) ||
    findString(value, ["email", "display_name", "displayName", "name"]) ||
    "Codex 官方账号";
  const credentialIdentity = credentialShape.officialToken || credentialShape.runtimeId || name;
  const identity = accountId || `mock_${accountIdHash(credentialIdentity)}`;
  const id = normalizeMockProviderId(providerId) || `codex_openai_${sanitizeProviderId(name)}_${accountIdHash(identity)}`;
  return {
    index,
    label: redactMockReviewText(providerImportItemLabel(value, index)),
    providerId: id,
    providerName: redactMockReviewText(name),
    providerKind: "official_codex",
    importKind: credentialShape.hasAgentIdentity ? "agent_identity" : "openai_account",
    credentialKind: mockOfficialCredentialKind(credentialShape),
    baseUrl: "https://chatgpt.com/backend-api/codex",
    websocketUrl: null,
    model: redactOptionalMockReviewText(findString(value, ["model", "defaultModel", "default_model"])),
    willOverwrite: Boolean(mockStatus.config.providers[id]),
  };
}

type MockProviderCredentialShape = {
  authMode: string | null;
  hasAgentIdentity: boolean;
  hasApiKeyShape: boolean;
  isOfficialPat: boolean;
  officialToken: string | null;
  privateKey: string | null;
  runtimeId: string | null;
};

function mockProviderCredentialShape(value: unknown): MockProviderCredentialShape {
  const authMode = findString(value, ["codex_companion_auth_mode", "auth_mode", "authMode"])?.toLowerCase() ?? null;
  const runtimeId = findString(value, ["agent_runtime_id", "agentRuntimeId"]);
  const privateKey = findString(value, ["agent_private_key", "agentPrivateKey"]);
  const hasAgentIdentity = authMode === "agentidentity" || authMode === "agent_identity" || Boolean(runtimeId && privateKey);
  const officialToken = findString(value, [
    "access_token",
    "accessToken",
    "personal_access_token",
    "personalAccessToken",
    "pat",
    "token",
    "refresh_token",
    "refreshToken",
    "id_token",
    "idToken",
  ]);
  const hasExplicitPatToken = Boolean(findString(value, ["personal_access_token", "personalAccessToken", "pat"]));
  const isOfficialPat = Boolean(officialToken) && (isExplicitOfficialPatMode(authMode) || hasExplicitPatToken);
  const hasApiKeyShape = !isOfficialPat && (
    authMode === "apikey" ||
    authMode === "api_key" ||
    isApiKeyJson(value) ||
    isNewApiChannelConnection(value) ||
    Boolean(findApiKey(value))
  );
  return {
    authMode,
    hasAgentIdentity,
    hasApiKeyShape,
    isOfficialPat,
    officialToken,
    privateKey,
    runtimeId,
  };
}

function mockOfficialAuthMode(credentialShape: MockProviderCredentialShape): string {
  if (credentialShape.hasAgentIdentity) {
    return "agentIdentity";
  }
  if (credentialShape.isOfficialPat) {
    return "pat";
  }
  return "oauth";
}

function mockOfficialCredentialKind(credentialShape: MockProviderCredentialShape): string {
  if (credentialShape.hasAgentIdentity) {
    return "Agent Identity 私钥";
  }
  if (credentialShape.isOfficialPat) {
    return "Personal access token";
  }
  return "OAuth tokens";
}

function reviewApiKeyJsonMock(
  value: unknown,
  index: number,
  providerId?: string,
  providerName?: string,
): ProviderImportReviewItem {
  const isNewApiConnection = isNewApiChannelConnection(value);
  const apiKey = findApiKey(value);
  if (!apiKey) {
    throw new Error(isNewApiConnection ? "New API 连接 JSON 缺少 key" : "API Key JSON 缺少 OPENAI_API_KEY");
  }
  const detectedBaseUrl = findApiBaseUrl(value);
  if (isNewApiConnection && !detectedBaseUrl) {
    throw new Error("New API 连接 JSON 缺少 url");
  }
  const baseUrl = detectedBaseUrl ?? "https://api.openai.com/v1";
  if (!/^https?:\/\//i.test(baseUrl)) {
    throw new Error(`API Key JSON 的 api_base_url 无效: ${baseUrl}`);
  }
  const name =
    emptyToNull(providerName) ||
    findString(value, ["api_provider_name", "apiProviderName", "provider_name", "providerName", "name"]) ||
    providerNameFromBaseUrl(baseUrl);
  const id =
    normalizeMockProviderId(providerId) ||
    normalizeMockProviderId(findString(value, ["api_provider_id", "apiProviderId"])) ||
    `${sanitizeProviderId(name)}_${accountIdHash(baseUrl)}`;
  const providerHint = `${id} ${name}`.toLowerCase();
  const usesRelay =
    providerHint.includes("new_api") ||
    providerHint.includes("new-api") ||
    providerHint.includes("one-api") ||
    !baseUrl.toLowerCase().startsWith("https://api.openai.com/");
  return {
    index,
    label: redactMockReviewText(providerImportItemLabel(value, index)),
    providerId: id,
    providerName: redactMockReviewText(name),
    providerKind: usesRelay ? "relay_provider" : "openai_compatible",
    importKind: "api_key",
    credentialKind: "API Key",
    baseUrl: redactMockReviewText(baseUrl),
    websocketUrl: redactOptionalMockReviewText(findString(value, ["websocket_url", "websocketUrl", "ws_url", "wsUrl"])),
    model: redactOptionalMockReviewText(findString(value, ["model", "defaultModel", "default_model"])),
    willOverwrite: Boolean(mockStatus.config.providers[id]),
  };
}

function providerImportItemLabel(value: unknown, index: number): string {
  return findString(value, ["email", "name", "chatgpt_user_id", "user_id", "chatgpt_account_id", "account_id"]) || `账号 ${index + 1}`;
}

function redactOptionalMockReviewText(value: string | null): string | null {
  return value ? redactMockReviewText(value) : null;
}

function redactMockReviewText(value: string): string {
  return value
    .replace(/\b(Bearer|AgentAssertion)\s+[A-Za-z0-9._~+/=-]+/gi, "$1 [redacted]")
    .replace(/\b(authorization|(?:set-)?cookie|(?:[a-z0-9-]+-)?api-key|x-auth-token)\s*:\s*[^\r\n]+/gi, "$1: [redacted]")
    .replace(/((?:^|[?&{,\s])["']?(?:(?:(?:access|refresh|id|session|auth)[_-]?)?token|(?:openai[_-]?)?api[_-]?key|key|authorization|cookie|client[_-]?secret|password|private[_-]?key)["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,}&]+)/gi, "$1[redacted]")
    .replace(/\b((?:https?|wss?):\/\/)[^/\s:@]+:[^/\s@]+@/gi, "$1[redacted]@")
    .replace(/\b(?:(?:sk|at)-[A-Za-z0-9][A-Za-z0-9._-]{7,}|cc_live_[A-Za-z0-9_-]{8,})\b/g, "[redacted]");
}

function importApiKeyJsonMock(
  value: unknown,
  providerId?: string,
  providerName?: string,
): Promise<ProviderImportOutcome> {
  const isNewApiConnection = isNewApiChannelConnection(value);
  const apiKey = findApiKey(value);
  if (!apiKey) {
    const message = isNewApiConnection
      ? "New API 连接 JSON 缺少 key"
      : "API Key JSON 缺少 OPENAI_API_KEY";
    return Promise.reject(new Error(message));
  }
  const detectedBaseUrl = findApiBaseUrl(value);
  if (isNewApiConnection && !detectedBaseUrl) {
    return Promise.reject(new Error("New API 连接 JSON 缺少 url"));
  }
  const baseUrl = detectedBaseUrl ?? "https://api.openai.com/v1";
  const name =
    emptyToNull(providerName) ||
    findString(value, ["api_provider_name", "apiProviderName", "provider_name", "providerName", "name"]) ||
    providerNameFromBaseUrl(baseUrl);
  const id =
    normalizeMockProviderId(providerId) ||
    normalizeMockProviderId(findString(value, ["api_provider_id", "apiProviderId"])) ||
    `${sanitizeProviderId(name)}_${accountIdHash(baseUrl)}`;
  const model = findString(value, ["model", "defaultModel", "default_model"]);
  const provider: ProviderConfig = {
    id,
    name,
    kind: baseUrl.startsWith("https://api.openai.com/") ? "openai_compatible" : "relay_provider",
    baseUrl,
    authRef: `file:${mockStatus.dataDir}/auth/api-keys/${id}.json`,
    directAuthRef: null,
    modelMap: model ? { [model]: model } : {},
    priority: 100,
    enabled: true,
    refreshIntervalSeconds: 60,
    account: {
      displayName: name,
      email: findString(value, ["email"]),
      subscriptionType: "API Key",
      subscriptionStatus: "待检查",
    },
  };
  const created = !mockStatus.config.providers[id];
  mockStatus = {
    ...mockStatus,
    config: {
      ...mockStatus.config,
      providers: {
        ...mockStatus.config.providers,
        [id]: provider,
      },
      health: {
        ...mockStatus.config.health,
        [id]: {
          status: "unknown",
          lastChecked: null,
          lastSuccess: null,
          lastError: null,
          lastFailureKind: null,
          cooldownUntil: null,
          failureCount: 0,
        },
      },
    },
  };
  mockStatus = syncMockDerived(mockStatus);
  return Promise.resolve<ProviderImportOutcome>({
    provider,
    importKind: "api_key",
    accountId: "api_key",
    authPath: `${mockStatus.dataDir}/auth/api-keys/${id}.json`,
    created,
    message: created ? "已导入 API Key provider" : "已更新 API Key provider",
  });
}

function sanitizeProviderId(value: string) {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .replace(/^([^a-z])/, "provider_$1") || "chatgpt";
}

function normalizeMockProviderId(value?: string | null): string | null {
  const normalized = emptyToNull(value ?? undefined);
  return normalized ? sanitizeProviderId(normalized) : null;
}

function accountIdHash(value: string) {
  let hash = 0;
  for (const char of value) {
    hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  }
  return hash.toString(16).padStart(8, "0").slice(0, 8);
}

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}

function providerCanDirectConnect(provider: ProviderConfig) {
  if (provider.kind === "official_codex" && officialProviderUsesOAuth(provider)) return false;
  if (provider.account?.authMode?.trim().toLowerCase() === "agentidentity") return false;
  if (providerEndpointIsChatCompletions(provider.baseUrl)) return false;
  const authRef = directAuthRef(provider);
  return !authRef || authRef.startsWith("env:") || authRef.startsWith("file:");
}

function officialProviderUsesOAuth(provider: ProviderConfig) {
  if (provider.kind !== "official_codex") return false;
  const mode = provider.account?.authMode?.trim().toLowerCase();
  return !mode || !isOfficialPatMode(mode);
}

function directAuthRef(provider: ProviderConfig): string {
  const authRef = provider.authRef?.trim();
  const directAuthRef = provider.directAuthRef?.trim();
  if (provider.kind === "official_codex") {
    return authRef || directAuthRef || "";
  }
  return directAuthRef || authRef || "";
}

function isOfficialPatMode(mode?: string): boolean {
  return ["pat", "personal_access_token", "personalaccesstoken", "token", "apikey", "api_key"].includes(mode || "");
}

function isExplicitOfficialPatMode(mode?: string | null): boolean {
  return ["pat", "personal_access_token", "personalaccesstoken", "token"].includes(mode || "");
}

function mockLaunchOutcome(
  mode: CodexLaunchOutcome["mode"],
  targetId: string,
  targetProviderId: string,
  codexDir?: string,
  restartRequired = mode === "provider_direct",
): CodexLaunchOutcome {
  const codexStarted = restartRequired;
  let message = `已切换本地代理到 ${targetId}，Codex 已在本地代理模式运行，无需重启`;
  if (mode === "provider_direct") {
    message = `已直连启动 ${targetId}，并已重启 ChatGPT / Codex 以载入账号/API Key`;
  } else if (restartRequired) {
    message = `已通过本地代理启动 ${targetId}，并已重启 ChatGPT / Codex`;
  }
  return {
    mode,
    targetId,
    targetProviderId,
    codex: mockStatus.codex,
    repair: {
      plan: {
        codexDir: codexDir || mockStatus.codex.codexDir,
        targetProviderId,
        historyFiles: 2,
        historyLines: 2,
        pluginFiles: 1,
        stateRows: 3,
        sourceProviderIds: ["openai", "custom"],
        dryRun: false,
      },
      backupRoot: `${mockStatus.codex.codexDir}/backups/codex-companion/mock`,
      migratedHistoryFiles: 2,
      migratedHistoryLines: 2,
      migratedPluginFiles: 1,
      migratedStateRows: 3,
      skippedReason: null,
    },
    restartRequired,
    codexStarted,
    message,
  };
}

function mockRelayRestartRequired() {
  const app = mockStatus.config.app;
  return (
    !mockStatus.codex.installed ||
    app.codexRestartRequiredOnNextRelay === true ||
    app.lastCodexLaunchMode === "provider_direct"
  );
}

function recordMockCodexLaunch(mode: CodexLaunchOutcome["mode"], targetProviderId: string) {
  mockStatus = {
    ...mockStatus,
    config: {
      ...mockStatus.config,
      app: {
        ...mockStatus.config.app,
        lastCodexLaunchMode: mode,
        lastCodexTargetProviderId: targetProviderId,
        codexRestartRequiredOnNextRelay: false,
      },
    },
  };
}

function createMockStatus(): CompanionStatus {
  const now = Date.now();
  const app = readStoredAppSettings();
  const providers: Record<string, ProviderConfig> = {
    "official-team": {
      id: "official-team",
      name: "Official Codex",
      kind: "official_codex",
      baseUrl: "https://chatgpt.com/backend-api/codex",
      websocketUrl: "wss://chatgpt.com/backend-api/codex/responses",
      authRef: `file:${MOCK_DATA_DIR}/auth/official-team.json`,
      directAuthRef: null,
      modelMap: {},
      priority: 100,
      enabled: true,
      refreshIntervalSeconds: 300,
      account: {
        authMode: "oauth",
        displayName: "alex@example.com",
        email: "alex@example.com",
        subscriptionType: "Plus",
        subscriptionStatus: "可用",
      },
    },
    "backup-api": {
      id: "backup-api",
      name: "Backup API",
      kind: "openai_compatible",
      baseUrl: "https://api.openai.com/v1",
      websocketUrl: "wss://api.openai.com/v1/responses",
      authRef: "env:OPENAI_API_KEY",
      directAuthRef: "env:OPENAI_API_KEY",
      modelMap: {},
      priority: 90,
      enabled: true,
      refreshIntervalSeconds: 300,
      account: {
        authMode: "apikey",
        displayName: "Backup API",
        subscriptionType: "API Key",
        subscriptionStatus: "可用",
      },
    },
  };
  const healthy = {
    status: "healthy",
    lastChecked: new Date().toISOString(),
    lastSuccess: new Date().toISOString(),
    lastError: null,
    lastFailureKind: null,
    cooldownUntil: null,
    failureCount: 0,
  };
  return syncMockDerived({
    config: {
      relay: {
        host: "127.0.0.1",
        port: 17687,
        activeGroupId: "default",
        requireApiKey: false,
        retryBudget: 0,
        modelCooldownSeconds: 300,
        sessionAffinityTtlSeconds: 3600,
        requestLogRetentionDays: 30,
      },
      providers,
      groups: {
        default: {
          id: "default",
          name: "Default",
          policy: "priority_fallback",
          providerOrder: ["official-team", "backup-api"],
          providerWeights: { "official-team": 3, "backup-api": 1 },
          fallbackEnabled: true,
          priorityFailbackIntervalSeconds: 0,
          priorityFailbackRevision: 0,
          priorityFailbackTargetProviderId: null,
        },
      },
      health: {
        "official-team": healthy,
        "backup-api": healthy,
      },
      app,
    },
    dataDir: MOCK_DATA_DIR,
    configPath: `${MOCK_DATA_DIR}/config.json`,
    relayBaseUrl: "http://127.0.0.1:17687/v1",
    activeGroup: null,
    activeProviders: [],
    codex: {
      codexDir: MOCK_CODEX_DIR,
      configPath: `${MOCK_CODEX_DIR}/config.toml`,
      installed: false,
      modelProvider: null,
      companionBaseUrl: "http://127.0.0.1:17687/v1",
      message: "Codex 配置存在，但尚未设置 model_provider",
    },
    recentEvents: [
      {
        timestamp: new Date(now - 8 * 60 * 1000).toISOString(),
        kind: "request",
        providerId: null,
        message: "[cc-demo-02] POST /v1/responses",
      },
      {
        timestamp: new Date(now - 8 * 60 * 1000 + 620).toISOString(),
        kind: "fallback",
        providerId: "official-team",
        message: "[cc-demo-02] 上游返回 503 Service Unavailable: upstream overloaded",
      },
      {
        timestamp: new Date(now - 8 * 60 * 1000 + 1_240).toISOString(),
        kind: "stream",
        providerId: "backup-api",
        message: "[cc-demo-02] POST /v1/responses -> 200 OK",
      },
      {
        timestamp: new Date(now - 90 * 1000).toISOString(),
        kind: "request",
        providerId: null,
        message: "[cc-demo-01] POST /v1/responses",
      },
      {
        timestamp: new Date(now - 90 * 1000 + 842).toISOString(),
        kind: "stream",
        providerId: "official-team",
        message: "[cc-demo-01] POST /v1/responses -> 200 OK",
      },
    ],
    dataRoots: {
      companionIsolated: false,
      codexIsolated: false,
    },
  });
}

function createMockModelMatrix(): ModelMatrixSnapshot {
  const generatedAt = new Date().toISOString();
  return {
    generatedAt,
    sources: [
      {
        id: "local-cache",
        name: "本地官方缓存",
        kind: "local_cache",
        providerId: null,
        activeGroup: false,
        status: "available",
        modelCount: 3,
        fetchedAt: new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString(),
        error: null,
      },
      {
        id: "provider:official-team",
        name: "Official Codex",
        kind: "official_oauth",
        providerId: "official-team",
        activeGroup: true,
        status: "available",
        modelCount: 3,
        fetchedAt: generatedAt,
        error: null,
      },
      {
        id: "provider:backup-api",
        name: "Backup API",
        kind: "relay",
        providerId: "backup-api",
        activeGroup: true,
        status: "available",
        modelCount: 3,
        fetchedAt: generatedAt,
        error: null,
      },
    ],
    models: [
      {
        id: "gpt-5.6-sol",
        displayName: "GPT-5.6-Sol",
        sourceIds: ["local-cache", "provider:official-team", "provider:backup-api"],
        reasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
        multiAgentVersion: "v2",
        ultraCapable: true,
        visibility: "list",
      },
      {
        id: "gpt-5.6-terra",
        displayName: "GPT-5.6-Terra",
        sourceIds: ["local-cache", "provider:official-team"],
        reasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"],
        multiAgentVersion: "v2",
        ultraCapable: true,
        visibility: "list",
      },
      {
        id: "gpt-5.5",
        displayName: "GPT-5.5",
        sourceIds: ["local-cache", "provider:official-team", "provider:backup-api"],
        reasoningEfforts: ["low", "medium", "high", "xhigh"],
        multiAgentVersion: null,
        ultraCapable: false,
        visibility: "list",
      },
      {
        id: "relay-preview-model",
        displayName: "relay-preview-model",
        sourceIds: ["provider:backup-api"],
        reasoningEfforts: [],
        multiAgentVersion: null,
        ultraCapable: false,
        visibility: null,
      },
    ],
  };
}

function createMockApiServiceSnapshot(): ApiServiceSnapshot {
  const now = Date.now();
  return {
    clients: [
      {
        id: "client_local_cli",
        name: "本地开发 CLI",
        keyPrefix: "cc_live_demo_8f1",
        allowedModels: ["gpt-5.6", "gpt-5.6-codex"],
        enabled: true,
        createdAt: new Date(now - 7 * 24 * 60 * 60 * 1000).toISOString(),
        lastUsedAt: new Date(now - 90 * 1000).toISOString(),
        requestCount: 128,
        usage: {
          today: { requests: 12, succeeded: 11, failed: 1, successRate: 91, averageLatencyMs: 842 },
          week: { requests: 48, succeeded: 46, failed: 2, successRate: 95, averageLatencyMs: 910 },
          month: { requests: 128, succeeded: 123, failed: 5, successRate: 96, averageLatencyMs: 980 },
        },
        health: { status: "healthy", lastRequestAt: new Date(now - 90 * 1000).toISOString(), lastSuccessAt: new Date(now - 90 * 1000).toISOString(), lastFailureAt: null, consecutiveFailures: 0 },
      },
    ],
    recentRequests: [
      {
        requestId: "cc-demo-01",
        startedAt: new Date(now - 90 * 1000).toISOString(),
        method: "POST",
        path: "/v1/responses",
        model: "gpt-5.6-codex",
        reasoningEffort: "high",
        serviceTier: "priority",
        clientId: "client_local_cli",
        clientName: "本地开发 CLI",
        providerId: "official-team",
        statusCode: 200,
        outcome: "succeeded",
        attempts: 1,
        latencyMs: 842,
        error: null,
        attemptLog: [
          {
            attempt: 1,
            providerId: "official-team",
            routeReason: "affinity",
            startedAt: new Date(now - 90 * 1000).toISOString(),
            finishedAt: new Date(now - 90 * 1000 + 842).toISOString(),
            statusCode: 200,
            outcome: "succeeded",
            latencyMs: 842,
            error: null,
          },
        ],
      },
      {
        requestId: "cc-demo-02",
        startedAt: new Date(now - 8 * 60 * 1000).toISOString(),
        method: "POST",
        path: "/v1/responses",
        model: "gpt-5.6",
        reasoningEffort: "medium",
        serviceTier: "default",
        clientId: "client_local_cli",
        clientName: "本地开发 CLI",
        providerId: "backup-api",
        statusCode: 200,
        outcome: "succeeded",
        attempts: 2,
        latencyMs: 1240,
        error: null,
        attemptLog: [
          {
            attempt: 1,
            providerId: "official-team",
            routeReason: "policy",
            startedAt: new Date(now - 8 * 60 * 1000).toISOString(),
            finishedAt: new Date(now - 8 * 60 * 1000 + 620).toISOString(),
            statusCode: 503,
            outcome: "failed",
            latencyMs: 620,
            error: "上游返回 503 Service Unavailable: upstream overloaded",
          },
          {
            attempt: 2,
            providerId: "backup-api",
            routeReason: "fallback",
            startedAt: new Date(now - 8 * 60 * 1000 + 620).toISOString(),
            finishedAt: new Date(now - 8 * 60 * 1000 + 1_240).toISOString(),
            statusCode: 200,
            outcome: "succeeded",
            latencyMs: 620,
            error: null,
          },
        ],
      },
    ],
    modelCooldowns: [],
    affinityBindings: 2,
    poolHealth: { total: 2, enabled: 2, healthy: 2, degraded: 0, cooldown: 0 },
  };
}

function emptyApiClientUsage() {
  return {
    today: { requests: 0, succeeded: 0, failed: 0, successRate: 0, averageLatencyMs: null },
    week: { requests: 0, succeeded: 0, failed: 0, successRate: 0, averageLatencyMs: null },
    month: { requests: 0, succeeded: 0, failed: 0, successRate: 0, averageLatencyMs: null },
  };
}

function defaultAppSettings(): AppSettings {
  return {
    theme: "light",
    providerViewMode: "compact",
    providerLaunchModes: {},
    lastCodexLaunchMode: null,
    lastCodexTargetProviderId: null,
    codexRestartRequiredOnNextRelay: false,
    preserveOfficialCodexAuth: false,
    tokenUsageRefreshIntervalSeconds: 30,
    preferredTerminal: "auto",
    recentWorkingDirectories: [],
  };
}

function readStoredAppSettings(): AppSettings {
  const fallback = defaultAppSettings();
  if (typeof localStorage === "undefined") return fallback;
  try {
    const raw = localStorage.getItem(APP_PREFS_STORAGE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<AppSettings>;
    return {
      theme: parsed.theme === "dark" || parsed.theme === "system" || parsed.theme === "light" ? parsed.theme : fallback.theme,
      providerViewMode: parsed.providerViewMode === "cards" || parsed.providerViewMode === "compact" ? parsed.providerViewMode : fallback.providerViewMode,
      providerLaunchModes: parsed.providerLaunchModes && typeof parsed.providerLaunchModes === "object" ? parsed.providerLaunchModes : fallback.providerLaunchModes,
      lastCodexLaunchMode: isCodexLaunchMode(parsed.lastCodexLaunchMode) ? parsed.lastCodexLaunchMode : fallback.lastCodexLaunchMode,
      lastCodexTargetProviderId:
        typeof parsed.lastCodexTargetProviderId === "string" ? parsed.lastCodexTargetProviderId : fallback.lastCodexTargetProviderId,
      codexRestartRequiredOnNextRelay:
        typeof parsed.codexRestartRequiredOnNextRelay === "boolean" ? parsed.codexRestartRequiredOnNextRelay : fallback.codexRestartRequiredOnNextRelay,
      preserveOfficialCodexAuth:
        typeof parsed.preserveOfficialCodexAuth === "boolean" ? parsed.preserveOfficialCodexAuth : fallback.preserveOfficialCodexAuth,
      tokenUsageRefreshIntervalSeconds:
        typeof parsed.tokenUsageRefreshIntervalSeconds === "number"
          ? parsed.tokenUsageRefreshIntervalSeconds
          : fallback.tokenUsageRefreshIntervalSeconds,
      preferredTerminal: parsed.preferredTerminal ?? fallback.preferredTerminal,
      recentWorkingDirectories: Array.isArray(parsed.recentWorkingDirectories)
        ? parsed.recentWorkingDirectories.filter((path): path is string => typeof path === "string")
        : fallback.recentWorkingDirectories,
    };
  } catch {
    return fallback;
  }
}

function mockCliCommand(input: CliLaunchRequest): string {
  const directory = `'${input.workingDirectory.replaceAll("'", `'\\''`)}'`;
  const command = input.resumeSessionId
    ? `codex resume '${input.resumeSessionId.replaceAll("'", `'\\''`)}'`
    : "codex";
  return `cd ${directory} && ${command}`;
}

function writeStoredAppSettings(settings: AppSettings) {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(APP_PREFS_STORAGE_KEY, JSON.stringify(settings));
}

function isCodexLaunchMode(value: unknown): value is CodexLaunchOutcome["mode"] {
  return value === "group_relay" || value === "provider_direct" || value === "provider_relay";
}

function syncMockDerived(status: CompanionStatus): CompanionStatus {
  const activeGroup = status.config.groups[status.config.relay.activeGroupId] ?? null;
  const activeProviders = activeGroup
    ? activeGroup.providerOrder
        .map((id) => status.config.providers[id])
        .filter((provider): provider is ProviderConfig => Boolean(provider))
    : [];
  const directConnectProviderIds = Object.values(status.config.providers)
    .filter(providerCanDirectConnect)
    .map((provider) => provider.id);
  return {
    ...status,
    activeGroup,
    activeProviders,
    directConnectProviderIds,
  };
}
