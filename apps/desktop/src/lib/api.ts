import { invoke } from "@tauri-apps/api/core";
import type {
  ApiKeyProviderUpdate,
  AppSettings,
  CodexLaunchOutcome,
  CodexInstallStatus,
  CompanionStatus,
  GroupUpsert,
  ProviderConfig,
  ProviderExportFormat,
  ProviderExportOutput,
  ProviderHealth,
  ProviderImportOutcome,
  ProviderGroup,
  ProviderKind,
  ProviderLaunchMode,
  ProviderViewMode,
  ProviderUpsert,
  RepairOutcome,
  ThemeMode,
} from "../types/domain";
import { extractQuotaWindows, findNumber, findString, isApiKeyJson, lowestQuotaPercent, providerNameFromBaseUrl } from "./provider-json";
import { getTokenUsage as getTokenUsageFromRuntime } from "./token-usage-api";

const APP_PREFS_STORAGE_KEY = "codex-companion:app-settings";
const MOCK_HOME_DIR = "/mock-home";
const MOCK_DATA_DIR = `${MOCK_HOME_DIR}/.codex-companion`;
const MOCK_CODEX_DIR = `${MOCK_HOME_DIR}/.codex`;

let mockStatus = createMockStatus();

export function getStatus() {
  if (!isTauri()) return Promise.resolve(mockStatus);
  return invoke<CompanionStatus>("get_status");
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
    const provider: ProviderConfig = {
      ...existing,
      name: input.providerName.trim(),
      kind: input.kind,
      baseUrl: input.baseUrl.trim().replace(/\/+$/, ""),
      authRef,
      directAuthRef,
      refreshIntervalSeconds: input.refreshIntervalSeconds || existing.refreshIntervalSeconds,
      account: {
        ...(existing.account ?? {}),
        email: input.providerDisplayName?.trim() || existing.account?.email || null,
        displayName: input.providerName.trim(),
        subscriptionType: "API Key",
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
  apiKey: string;
  envVar?: string;
  model?: string;
  refreshIntervalSeconds?: number;
}) {
  if (!isTauri()) {
    const id = `${sanitizeProviderId(input.providerName)}_${accountIdHash(input.baseUrl)}`;
    const provider: ProviderConfig = {
      id,
      name: input.providerName,
      kind: input.kind,
      baseUrl: input.baseUrl.replace(/\/+$/, ""),
      authRef: input.apiKey ? `file:${mockStatus.dataDir}/auth/api-keys/${id}.json` : input.envVar ? `env:${input.envVar}` : null,
      directAuthRef: input.envVar ? `env:${input.envVar}` : null,
      modelMap: input.model ? { [input.model]: input.model } : {},
      priority: 100,
      enabled: true,
      refreshIntervalSeconds: input.refreshIntervalSeconds || 60,
      account: {
        displayName: input.providerName,
        subscriptionType: "API Key",
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
    providerName: input.providerName,
    kind: input.kind,
    baseUrl: input.baseUrl,
    apiKey: input.apiKey,
    envVar: emptyToNull(input.envVar),
    model: emptyToNull(input.model),
    refreshIntervalSeconds: input.refreshIntervalSeconds ?? null,
  });
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
    if (isApiKeyJson(value)) {
      return importApiKeyJsonMock(value, providerId, providerName);
    }
    const name =
      emptyToNull(providerName) ||
      findString(value, ["name", "email", "provider_name", "providerName"]) ||
      "Codex 官方账号";
    const accountId =
      findString(value, ["chatgpt_account_id", "account_id", "workspace_id", "email"]) ||
      `mock_${Date.now()}`;
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
    const id = emptyToNull(providerId) || `codex_openai_${sanitizeProviderId(name)}_${accountIdHash(accountId)}`;
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
    return Promise.resolve<ProviderImportOutcome>({
      provider,
      importKind: "openai_account",
      accountId,
      authPath: `${mockStatus.dataDir}/auth/accounts/${id}.json`,
      created,
      message: created ? "已导入 Codex 官方账号 provider" : "已更新 Codex 官方账号 provider",
    });
  }
  return invoke<ProviderImportOutcome>("import_provider_json", {
    jsonText,
    providerId: emptyToNull(providerId),
    providerName: emptyToNull(providerName),
  });
}

export function importProviderJsonMany(jsonText: string, providerId?: string, providerName?: string) {
  if (!isTauri()) {
    const value = JSON.parse(jsonText) as unknown;
    if (Array.isArray(value) && value.length > 0) {
      return Promise.all(value.map((item) => importProviderJson(JSON.stringify(item), undefined, providerName)));
    }
    const accounts =
      value && typeof value === "object" && !Array.isArray(value)
        ? (value as { accounts?: unknown }).accounts
        : null;
    if (Array.isArray(accounts) && accounts.length > 1) {
      return Promise.all(
        accounts.map((account) => {
          const item = {
            ...(value as Record<string, unknown>),
            accounts: [account],
          };
          return importProviderJson(JSON.stringify(item), undefined, providerName);
        }),
      );
    }
    return importProviderJson(jsonText, providerId, providerName).then((outcome) => [outcome]);
  }
  return invoke<ProviderImportOutcome[]>("import_provider_json_many", {
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
    mockStatus = {
      ...mockStatus,
      config: {
        ...mockStatus.config,
        groups: {
          ...mockStatus.config.groups,
          [input.id]: input,
        },
      },
    };
    mockStatus = syncMockDerived(mockStatus);
    return Promise.resolve(input);
  }
  return invoke<ProviderGroup>("upsert_group", { input });
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
    const shouldDirect = mode === "direct" || (mode === "auto" && providerCanDirectConnect(provider));
    if (mode === "direct" && !providerCanDirectConnect(provider)) {
      return Promise.reject(new Error(`${provider.name} 缺少直连所需的账号材料、API Key 文件或环境变量`));
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
      fallbackEnabled: false,
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

export function getTokenUsage(codexDir?: string) {
  return getTokenUsageFromRuntime(codexDir);
}

function emptyToNull(value?: string) {
  return value && value.trim() ? value.trim() : null;
}

function importApiKeyJsonMock(value: unknown, providerId?: string, providerName?: string) {
  const apiKey = findString(value, ["OPENAI_API_KEY", "openai_api_key", "openaiApiKey", "api_key", "apiKey"]) ?? "";
  const baseUrl = (
    findString(value, ["api_base_url", "apiBaseUrl", "base_url", "baseUrl"]) ?? "https://api.openai.com/v1"
  ).replace(/\/+$/, "");
  const name =
    emptyToNull(providerName) ||
    findString(value, ["api_provider_name", "apiProviderName", "provider_name", "providerName", "name"]) ||
    providerNameFromBaseUrl(baseUrl);
  const id =
    emptyToNull(providerId) ||
    findString(value, ["api_provider_id", "apiProviderId"]) ||
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
  const authRef = provider.directAuthRef?.trim() || provider.authRef?.trim();
  return !authRef || authRef.startsWith("env:") || authRef.startsWith("file:");
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
  const app = readStoredAppSettings();
  return syncMockDerived({
    config: {
      relay: {
        host: "127.0.0.1",
        port: 17687,
        activeGroupId: "default",
      },
      providers: {},
      groups: {
        default: {
          id: "default",
          name: "Default",
          policy: "priority_fallback",
          providerOrder: [],
          fallbackEnabled: true,
        },
      },
      health: {},
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
    recentEvents: [],
  });
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
    };
  } catch {
    return fallback;
  }
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
  return {
    ...status,
    activeGroup,
    activeProviders,
  };
}
