import { useCallback, useEffect, useRef, useState } from "react";
import {
  completeCodexOAuth,
  exportProviderJson,
  getStatus,
  getTokenUsage,
  importApiKeyProvider,
  importLocalCodexProvider,
  importProviderJsonMany,
  install,
  launchGroup,
  launchProvider,
  removeProvider,
  repair,
  refreshAllProviders as refreshAllProvidersApi,
  refreshProvider as refreshProviderApi,
  requestPriorityFailback,
  resetAppSettings,
  setPreserveOfficialCodexAuth,
  setProviderLaunchMode,
  setProviderViewMode,
  setTheme,
  setTokenUsageRefreshInterval,
  uninstall,
  updateApiKeyProvider,
  upsertGroup,
  useGroup,
} from "../lib/api";
import type {
  ApiKeyProviderUpdate,
  BusyState,
  CompanionStatus,
  GroupUpsert,
  ProviderExportFormat,
  ProviderExportOutput,
  ProviderLaunchMode,
  ProviderImportBatchReport,
  ProviderViewMode,
  RepairOutcome,
  ThemeMode,
} from "../types/domain";
import type { ApiKeyForm, JsonImportFile } from "../features/providers/provider-types";
import { useAppUpdater } from "../features/settings/useAppUpdater";
import { userFacingError } from "../lib/errors";

export function useCompanionController() {
  const [busy, setBusy] = useState<BusyState>("loading");
  const [status, setStatus] = useState<CompanionStatus | null>(null);
  const [repairOutcome, setRepairOutcome] = useState<RepairOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState("");
  const [activeTab, setActiveTab] = useState("dashboard");
  const [refreshingAllProviders, setRefreshingAllProviders] = useState(false);
  const [refreshingProviderIds, setRefreshingProviderIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const refreshInFlightRef = useRef<Promise<void> | null>(null);
  const refreshAllProvidersInFlightRef = useRef<Promise<void> | null>(null);
  const providerRefreshesInFlightRef = useRef<Map<string, Promise<void>>>(new Map());
  const hasLoadedStatusRef = useRef(false);
  const appUpdater = useAppUpdater(setToast);

  const refresh = useCallback(async (options: { silent?: boolean; waitForInFlight?: boolean } = {}) => {
    while (refreshInFlightRef.current) {
      if (options.silent && !options.waitForInFlight) return;
      await refreshInFlightRef.current;
    }
    const refreshTask = (async (): Promise<void> => {
      if (!options.silent) {
        setBusy((current) => (current === "idle" ? "loading" : current));
      }
      try {
        const next = await getStatus();
        setStatus(next);
        hasLoadedStatusRef.current = true;
        setError(null);
      } catch (unknownError) {
        if (!options.silent || !hasLoadedStatusRef.current) {
          setError(userFacingError(unknownError));
        }
      } finally {
        if (!options.silent) {
          setBusy("idle");
        }
      }
    })();
    refreshInFlightRef.current = refreshTask;
    try {
      await refreshTask;
    } finally {
      if (refreshInFlightRef.current === refreshTask) {
        refreshInFlightRef.current = null;
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => {
      void refresh({ silent: true });
    }, 15_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    if (status) {
      applyTheme(status.config.app.theme);
    }
  }, [status]);

  async function run(
    label: string,
    state: BusyState,
    action: () => Promise<void | string>,
    options: { propagateError?: boolean } = {},
  ) {
    setBusy(state);
    setError(null);
    try {
      const nextLabel = await action();
      setToast(nextLabel || label);
      await refresh();
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      if (options.propagateError) {
        throw unknownError;
      }
    } finally {
      setBusy("idle");
    }
  }

  async function changeTheme(theme: ThemeMode) {
    await run("主题已更新", "saving", async () => {
      await setTheme(theme);
      applyTheme(theme);
    });
  }

  async function changeProviderViewMode(mode: ProviderViewMode) {
    setStatus((current) =>
      current
        ? {
            ...current,
            config: {
              ...current.config,
              app: {
                ...current.config.app,
                providerViewMode: mode,
              },
            },
          }
        : current,
    );
    try {
      await setProviderViewMode(mode);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      await refresh();
    }
  }

  async function changeProviderLaunchMode(providerId: string, mode: ProviderLaunchMode) {
    setStatus((current) =>
      current
        ? {
            ...current,
            config: {
              ...current.config,
              app: {
                ...current.config.app,
                providerLaunchModes: {
                  ...current.config.app.providerLaunchModes,
                  [providerId]: mode,
                },
              },
            },
          }
        : current,
    );
    try {
      await setProviderLaunchMode(providerId, mode);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      await refresh();
    }
  }

  async function changePreserveOfficialCodexAuth(preserve: boolean) {
    setStatus((current) =>
      current
        ? {
            ...current,
            config: {
              ...current.config,
              app: {
                ...current.config.app,
                preserveOfficialCodexAuth: preserve,
              },
            },
          }
        : current,
    );
    try {
      await setPreserveOfficialCodexAuth(preserve);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      await refresh();
    }
  }

  async function changeTokenUsageRefreshInterval(seconds: number) {
    setStatus((current) =>
      current
        ? {
            ...current,
            config: {
              ...current.config,
              app: {
                ...current.config.app,
                tokenUsageRefreshIntervalSeconds: seconds,
              },
            },
          }
        : current,
    );
    try {
      await setTokenUsageRefreshInterval(seconds);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      await refresh();
    }
  }

  async function importJsonBatch(
    jsonFiles: JsonImportFile[],
    addToGroupId?: string | null,
  ): Promise<ProviderImportBatchReport> {
    setBusy("saving");
    setError(null);
    const combined: ProviderImportBatchReport = {
      total: 0,
      succeeded: [],
      failed: [],
      addedToGroup: [],
    };
    try {
      for (const file of jsonFiles) {
        const offset = combined.total;
        try {
          const report = await importProviderJsonMany(file.text, undefined, undefined, addToGroupId);
          combined.total += report.total;
          combined.succeeded.push(...report.succeeded);
          combined.failed.push(
            ...report.failed.map((failure) => ({
              ...failure,
              index: failure.index + offset,
              label: `${file.name} · ${failure.label}`,
            })),
          );
          combined.addedToGroup.push(...report.addedToGroup);
        } catch (unknownError) {
          combined.total += 1;
          combined.failed.push({
            index: offset,
            label: file.name,
            message: userFacingError(unknownError),
          });
        }
      }
      const message = combined.failed.length
        ? `导入完成：${combined.succeeded.length} 成功，${combined.failed.length} 失败`
        : `已导入 ${combined.succeeded.length} 个账号`;
      setToast(message);
      await refresh();
      return combined;
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      throw unknownError;
    } finally {
      setBusy("idle");
    }
  }

  async function resetPreferences() {
    await run("界面偏好已恢复默认", "saving", async () => {
      const appSettings = await resetAppSettings();
      applyTheme(appSettings.theme);
    });
  }

  async function exportProvider(id: string, format?: ProviderExportFormat | null): Promise<ProviderExportOutput> {
    setBusy("saving");
    setError(null);
    try {
      return await exportProviderJson(id, format);
    } catch (unknownError) {
      setError(userFacingError(unknownError));
      throw unknownError;
    } finally {
      setBusy("idle");
    }
  }

  async function toggleTheme() {
    const current = status?.config.app.theme === "dark" ? "dark" : "light";
    await changeTheme(current === "dark" ? "light" : "dark");
  }

  async function performProviderRefresh(id: string): Promise<void> {
    setRefreshingProviderIds((current) => new Set(current).add(id));
    setError(null);
    try {
      await refreshProviderApi(id);
      setToast("Provider 状态已刷新");
      await refresh({ silent: true, waitForInFlight: true });
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setRefreshingProviderIds((current) => {
        if (!current.has(id)) {
          return current;
        }
        const next = new Set(current);
        next.delete(id);
        return next;
      });
    }
  }

  async function refreshProviderAccount(id: string): Promise<void> {
    const existing = providerRefreshesInFlightRef.current.get(id);
    if (existing) {
      await existing;
      return;
    }

    const task = performProviderRefresh(id);
    providerRefreshesInFlightRef.current.set(id, task);
    try {
      await task;
    } finally {
      if (providerRefreshesInFlightRef.current.get(id) === task) {
        providerRefreshesInFlightRef.current.delete(id);
      }
    }
  }

  async function performAllProvidersRefresh(): Promise<void> {
    setRefreshingAllProviders(true);
    setError(null);
    try {
      await refreshAllProvidersApi();
      setToast("Provider 状态已全部刷新");
      await refresh({ silent: true, waitForInFlight: true });
    } catch (unknownError) {
      setError(userFacingError(unknownError));
    } finally {
      setRefreshingAllProviders(false);
    }
  }

  async function refreshAllProviderAccounts(): Promise<void> {
    const existing = refreshAllProvidersInFlightRef.current;
    if (existing) {
      await existing;
      return;
    }

    const task = performAllProvidersRefresh();
    refreshAllProvidersInFlightRef.current = task;
    try {
      await task;
    } finally {
      if (refreshAllProvidersInFlightRef.current === task) {
        refreshAllProvidersInFlightRef.current = null;
      }
    }
  }

  const progress = busy === "idle" ? 0 : busy === "loading" ? 34 : busy === "repairing" ? 78 : busy === "launching" ? 88 : 58;

  return {
    activeTab,
    appUpdater,
    busy,
    error,
    progress,
    refreshingAllProviders,
    refreshingProviderIds,
    repairOutcome,
    status,
    toast,
    actions: {
      changeProviderLaunchMode,
      changeProviderViewMode,
      changePreserveOfficialCodexAuth,
      changeTokenUsageRefreshInterval,
      changeTheme,
      importApiKey: (input: ApiKeyForm) =>
        run("API Key Provider 已添加", "saving", async () => {
          await importApiKeyProvider(input);
        }, { propagateError: true }),
      updateApiKeyProvider: (input: ApiKeyProviderUpdate) =>
        run("API Key Provider 已更新", "saving", async () => {
          await updateApiKeyProvider(input);
        }, { propagateError: true }),
      exportProvider,
      importCodexOAuth: (loginId: string) =>
        run("OAuth 账号已添加", "saving", async () => {
          const outcome = await completeCodexOAuth(loginId);
          return outcome.message;
        }, { propagateError: true }),
      importJsonBatch,
      importLocal: () =>
        run("已导入本地 Codex 账号", "saving", async () => {
          await importLocalCodexProvider();
        }, { propagateError: true }),
      install: () =>
        run("已写入 Codex 启动配置", "saving", async () => {
          await install();
        }),
      launchGroup: (id: string) =>
        run("已按当前分组启动 Codex", "launching", async () => {
          const outcome = await launchGroup(id);
          return outcome.message;
        }),
      launchProvider: (id: string, mode?: ProviderLaunchMode) =>
        run("已按单 Provider 启动 Codex", "launching", async () => {
          const outcome = await launchProvider(id, mode ?? status?.config.app.providerLaunchModes[id] ?? "auto");
          return outcome.message;
        }),
      loadTokenUsage: getTokenUsage,
      refreshAllProviders: refreshAllProviderAccounts,
      refreshProvider: refreshProviderAccount,
      removeProvider: (id: string) =>
        run("Provider 已删除", "saving", async () => {
          await removeProvider(id);
        }),
      repair: async (
        history: boolean,
        plugins: boolean,
        dryRun: boolean,
        codexDir?: string,
        targetProviderId?: string,
      ) => {
        setError(null);
        try {
          const outcome = await repair(history, plugins, dryRun, codexDir, targetProviderId);
          setRepairOutcome(outcome);
          setToast(dryRun ? "Dry-run 已完成" : "修复已完成");
          void refresh({ silent: true });
        } catch (unknownError) {
          setError(userFacingError(unknownError));
        }
      },
      resetPreferences,
      requestPriorityFailback: (id: string, providerId: string) =>
        run("已安排活跃会话尝试指定 Provider", "saving", async () => {
          await requestPriorityFailback(id, providerId);
        }),
      saveGroup: (group: GroupUpsert) =>
        run("分组已保存", "saving", async () => {
          await upsertGroup(group);
        }, { propagateError: true }),
      setActiveTab,
      setToast,
      toggleTheme,
      uninstall: () =>
        run("已恢复 Codex 启动配置", "saving", async () => {
          await uninstall();
        }),
      useGroup: (id: string) =>
        run("当前分组已切换", "saving", async () => {
          await useGroup(id);
        }),
    },
  };
}

function applyTheme(theme: ThemeMode) {
  document.documentElement.dataset.theme = theme;
}
