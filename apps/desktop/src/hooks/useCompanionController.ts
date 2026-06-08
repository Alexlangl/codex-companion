import { useCallback, useEffect, useRef, useState } from "react";
import {
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
  refreshAllProviders,
  refreshProvider,
  resetAppSettings,
  setProviderLaunchMode,
  setProviderViewMode,
  setTheme,
  uninstall,
  upsertGroup,
  useGroup,
} from "../lib/api";
import type {
  BusyState,
  CompanionStatus,
  GroupUpsert,
  ProviderLaunchMode,
  ProviderViewMode,
  RepairOutcome,
  ThemeMode,
} from "../types/domain";
import type { ApiKeyForm, JsonImportFile } from "../features/providers/provider-types";

export function useCompanionController() {
  const [busy, setBusy] = useState<BusyState>("loading");
  const [status, setStatus] = useState<CompanionStatus | null>(null);
  const [repairOutcome, setRepairOutcome] = useState<RepairOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState("");
  const [activeTab, setActiveTab] = useState("dashboard");
  const pollingRef = useRef(false);
  const hasLoadedStatusRef = useRef(false);

  const refresh = useCallback(async (options: { silent?: boolean } = {}) => {
    if (options.silent && pollingRef.current) return;
    pollingRef.current = true;
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
        setError(String(unknownError));
      }
    } finally {
      pollingRef.current = false;
      if (!options.silent) {
        setBusy("idle");
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

  async function run(label: string, state: BusyState, action: () => Promise<void>) {
    setBusy(state);
    setError(null);
    try {
      await action();
      setToast(label);
      await refresh();
    } catch (unknownError) {
      setError(String(unknownError));
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
      setError(String(unknownError));
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
      setError(String(unknownError));
      await refresh();
    }
  }

  async function resetPreferences() {
    await run("界面偏好已恢复默认", "saving", async () => {
      const appSettings = await resetAppSettings();
      applyTheme(appSettings.theme);
    });
  }

  async function toggleTheme() {
    const current = status?.config.app.theme === "dark" ? "dark" : "light";
    await changeTheme(current === "dark" ? "light" : "dark");
  }

  const progress = busy === "idle" ? 0 : busy === "loading" ? 34 : busy === "repairing" ? 78 : busy === "launching" ? 88 : 58;

  return {
    activeTab,
    busy,
    error,
    progress,
    repairOutcome,
    status,
    toast,
    actions: {
      changeProviderLaunchMode,
      changeProviderViewMode,
      changeTheme,
      importApiKey: (input: ApiKeyForm) =>
        run("API Key Provider 已添加", "saving", async () => {
          await importApiKeyProvider(input);
        }),
      importJsonBatch: (jsonFiles: JsonImportFile[]) =>
        run(`已导入 ${jsonFiles.length} 个 JSON 文件`, "saving", async () => {
          for (const file of jsonFiles) {
            await importProviderJsonMany(file.text);
          }
        }),
      importLocal: () =>
        run("已导入本地 Codex 账号", "saving", async () => {
          await importLocalCodexProvider();
        }),
      install: () =>
        run("已写入 Codex 启动配置", "saving", async () => {
          await install();
        }),
      launchGroup: (id: string) =>
        run("已按当前分组启动 Codex", "launching", async () => {
          await launchGroup(id);
        }),
      launchProvider: (id: string, mode?: ProviderLaunchMode) =>
        run("已按单 Provider 启动 Codex", "launching", async () => {
          await launchProvider(id, mode ?? status?.config.app.providerLaunchModes[id] ?? "auto");
        }),
      loadTokenUsage: getTokenUsage,
      refreshAllProviders: () =>
        run("Provider 状态已全部刷新", "testing", async () => {
          await refreshAllProviders();
        }),
      refreshProvider: (id: string) =>
        run("Provider 状态已刷新", "testing", async () => {
          await refreshProvider(id);
        }),
      removeProvider: (id: string) =>
        run("Provider 已删除", "saving", async () => {
          await removeProvider(id);
        }),
      repair: (
        history: boolean,
        plugins: boolean,
        dryRun: boolean,
        codexDir?: string,
      ) =>
        run(dryRun ? "Dry-run 已完成" : "修复已完成", "repairing", async () => {
          const outcome = await repair(history, plugins, dryRun, codexDir);
          setRepairOutcome(outcome);
        }),
      resetPreferences,
      saveGroup: (group: GroupUpsert) =>
        run("分组已保存", "saving", async () => {
          await upsertGroup(group);
        }),
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
