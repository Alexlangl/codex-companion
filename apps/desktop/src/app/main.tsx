import * as Dialog from "@radix-ui/react-dialog";
import * as Progress from "@radix-ui/react-progress";
import * as Tabs from "@radix-ui/react-tabs";
import * as Toast from "@radix-ui/react-toast";
import * as Tooltip from "@radix-ui/react-tooltip";
import { Boxes, Command, Gauge, GitBranch, Hammer, History, LayoutDashboard, Moon, RadioTower, Settings as SettingsIcon, Sun, X } from "lucide-react";
import React from "react";
import { createRoot } from "react-dom/client";
import { Button, IconButton } from "../components/ui";
import { Dashboard } from "../features/dashboard/Dashboard";
import { Groups } from "../features/groups/Groups";
import { Providers } from "../features/providers/Providers";
import { canDirectLaunch, directLaunchWritesAuthJson } from "../features/providers/provider-launch";
import { Relay } from "../features/relay/Relay";
import { Repair } from "../features/repair/Repair";
import { Sessions } from "../features/sessions/Sessions";
import { AppUpdatePrompt } from "../features/settings/AppUpdatePrompt";
import { Settings } from "../features/settings/Settings";
import { TokenStats } from "../features/token/TokenStats";
import { useCompanionController } from "../hooks/useCompanionController";
import { providerAccountTitle } from "../lib/provider-display";
import "../styles/tokens.css";
import "../styles/base.css";
import "../styles/layout.css";
import "../styles/components.css";
import "../styles/radix.css";
import type { ProviderConfig, ProviderLaunchMode } from "../types/domain";
import { AppErrorBoundary } from "./AppErrorBoundary";

const root = document.querySelector<HTMLDivElement>("#app");

const pageTitles: Record<string, string> = {
  dashboard: "总览",
  providers: "账号",
  groups: "分组",
  relay: "转发",
  token: "用量",
  sessions: "会话",
  repair: "修复",
  settings: "设置",
};

if (!root) {
  throw new Error("Missing #app root");
}

function App() {
  const { activeTab, actions, appUpdater, busy, error, progress, repairOutcome, status, toast } = useCompanionController();
  const [pendingProviderLaunch, setPendingProviderLaunch] = React.useState<{
    mode: ProviderLaunchMode;
    provider: ProviderConfig;
  } | null>(null);
  const directLaunchDialogTitleRef = React.useRef<HTMLHeadingElement>(null);
  let directLaunchDescription = "直连会更新 Codex auth.json。";
  if (pendingProviderLaunch) {
    directLaunchDescription = `${providerAccountTitle(pendingProviderLaunch.provider)} 将把账号材料合并写入 Codex auth.json，并由 ChatGPT / Codex 重新载入。`;
  }

  async function requestLaunchProvider(id: string, mode?: ProviderLaunchMode) {
    if (!status) {
      await actions.launchProvider(id, mode);
      return;
    }
    const provider = status.config.providers[id];
    if (!provider) {
      await actions.launchProvider(id, mode);
      return;
    }
    const resolvedMode = mode ?? status.config.app.providerLaunchModes[id] ?? "auto";
    const willDirect = resolvedMode === "direct" || (resolvedMode === "auto" && canDirectLaunch(provider));
    if (willDirect && directLaunchWritesAuthJson(provider, status.config.app.preserveOfficialCodexAuth)) {
      setPendingProviderLaunch({ mode: resolvedMode, provider });
      return;
    }
    await actions.launchProvider(id, mode);
  }

  function confirmPendingProviderLaunch() {
    if (!pendingProviderLaunch) return;
    const { mode, provider } = pendingProviderLaunch;
    setPendingProviderLaunch(null);
    void actions.launchProvider(provider.id, mode);
  }

  return (
    <Toast.Provider swipeDirection="right">
      <Tooltip.Provider delayDuration={180}>
        {status ? (
          <Tabs.Root className="tabs-root app-tabs-root" onValueChange={actions.setActiveTab} value={activeTab}>
            <main className="app-shell">
              <aside className="sidebar">
                <Brand />
                <Tabs.List className="tabs-list sidebar-tabs" aria-label="Codex Companion">
                  <Tab value="dashboard" icon={<LayoutDashboard size={16} />} label="总览" />
                  <Tab value="providers" icon={<Boxes size={16} />} label="账号" />
                  <Tab value="groups" icon={<GitBranch size={16} />} label="分组" />
                  <Tab value="relay" icon={<RadioTower size={16} />} label="转发" />
                  <Tab value="token" icon={<Gauge size={16} />} label="用量" />
                  <Tab value="sessions" icon={<History size={16} />} label="会话" />
                  <Tab value="repair" icon={<Hammer size={16} />} label="修复" />
                  <Tab value="settings" icon={<SettingsIcon size={16} />} label="设置" />
                </Tabs.List>
                <div className="sidebar-status" aria-live="polite">
                  <span className={`status-dot ${busy === "idle" ? "status-dot-ok" : "status-dot-busy"}`} />
                  <span>{busy === "idle" ? "本地服务正常" : "正在处理"}</span>
                </div>
              </aside>

              <section className="workspace">
                <header className="topbar">
                  <h1>{pageTitles[activeTab] ?? "Companion"}</h1>
                  <div className="topbar-actions">
                    <IconButton
                      label={status.config.app.theme === "dark" ? "切换到亮色主题" : "切换到暗色主题"}
                      onClick={() => void actions.toggleTheme()}
                    >
                      {status.config.app.theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
                    </IconButton>
                  </div>
                </header>

                <Progress.Root className="progress-root" value={progress}>
                  <Progress.Indicator
                    className="progress-indicator"
                    style={{ transform: `translateX(-${100 - progress}%)` }}
                  />
                </Progress.Root>

                {error ? <div className="error-banner">{error}</div> : null}

                <Tabs.Content className="tabs-content" value="dashboard">
                  <Dashboard busy={busy} status={status} onLaunchGroup={actions.launchGroup} onLaunchProvider={requestLaunchProvider} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="providers">
                  <Providers
                    busy={busy}
                    status={status}
                    viewMode={status.config.app.providerViewMode}
                    launchModes={status.config.app.providerLaunchModes}
                    onLaunchModeChange={actions.changeProviderLaunchMode}
                    onViewModeChange={actions.changeProviderViewMode}
                    onExport={actions.exportProvider}
                    onImportApiKey={actions.importApiKey}
                    onImportJsonBatch={actions.importJsonBatch}
                    onImportLocal={actions.importLocal}
                    onRemove={actions.removeProvider}
                    onRefresh={actions.refreshProvider}
                    onRefreshAll={actions.refreshAllProviders}
                    onUpdateApiKey={actions.updateApiKeyProvider}
                    onLaunch={requestLaunchProvider}
                  />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="groups">
                  <Groups
                    busy={busy}
                    status={status}
                    onSave={actions.saveGroup}
                    onUse={actions.useGroup}
                    onLaunch={actions.launchGroup}
                  />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="relay">
                  <Relay active={activeTab === "relay"} status={status} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="token">
                  <TokenStats active={activeTab === "token"} status={status} onLoad={actions.loadTokenUsage} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="sessions">
                  <Sessions active={activeTab === "sessions"} status={status} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="repair">
                  <Repair outcome={repairOutcome} status={status} onRepair={actions.repair} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="settings">
                  <Settings
                    appUpdater={appUpdater}
                    busy={busy}
                    status={status}
                    onInstall={actions.install}
                    onUninstall={actions.uninstall}
                    onPreserveOfficialCodexAuth={actions.changePreserveOfficialCodexAuth}
                    onTokenUsageRefreshInterval={actions.changeTokenUsageRefreshInterval}
                    onTheme={actions.changeTheme}
                    onResetPreferences={actions.resetPreferences}
                  />
                </Tabs.Content>
              </section>
            </main>
          </Tabs.Root>
        ) : (
          <main className="app-shell">
            <aside className="sidebar">
              <Brand />
            </aside>
            <section className="workspace">
              <div className="loading-panel">正在加载 Companion 状态</div>
            </section>
          </main>
        )}
        <AppUpdatePrompt isBlocked={Boolean(pendingProviderLaunch)} updater={appUpdater} />
        <Dialog.Root open={Boolean(pendingProviderLaunch)} onOpenChange={(open) => !open && setPendingProviderLaunch(null)}>
          <Dialog.Portal>
            <Dialog.Overlay className="dialog-overlay" />
            <Dialog.Content
              className="dialog-content direct-launch-confirm-dialog"
              onOpenAutoFocus={(event) => {
                event.preventDefault();
                directLaunchDialogTitleRef.current?.focus();
              }}
            >
              <div className="dialog-header">
                <div>
                  <Dialog.Title className="dialog-title" ref={directLaunchDialogTitleRef} tabIndex={-1}>
                    确认直连写入
                  </Dialog.Title>
                  <Dialog.Description className="dialog-description">
                    {directLaunchDescription}
                  </Dialog.Description>
                </div>
                <Dialog.Close className="icon-button" aria-label="关闭">
                  <X size={16} />
                </Dialog.Close>
              </div>
              <div className="warning-box">
                <strong>这会修改本机 Codex 登录材料</strong>
                <p>
                  Companion 会先备份并记录 ownership marker。开启“保留官方 Codex 登录”后，第三方 API key 会改写到 provider-scoped config.toml，不再修改官方 OAuth auth.json。
                </p>
              </div>
              <div className="actions">
                <Button disabled={busy !== "idle"} onClick={confirmPendingProviderLaunch} variant="danger">
                  确认直连
                </Button>
                <Dialog.Close asChild>
                  <Button disabled={busy !== "idle"} variant="secondary">
                    取消
                  </Button>
                </Dialog.Close>
              </div>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      </Tooltip.Provider>
      <Toast.Root className="toast-root" onOpenChange={() => actions.setToast("")} open={Boolean(toast)}>
        <Toast.Title className="toast-title">{toast}</Toast.Title>
      </Toast.Root>
      <Toast.Viewport className="toast-viewport" />
    </Toast.Provider>
  );
}

function Brand() {
  return (
    <div className="brand" aria-label="Codex Companion">
      <div className="brand-icon" aria-hidden="true">
        <Command size={16} strokeWidth={2.2} />
      </div>
      <div className="brand-copy">
        <strong>Companion</strong>
        <span>Codex 本地工具</span>
      </div>
    </div>
  );
}

function Tab({ value, icon, label }: { value: string; icon: React.ReactNode; label: string }) {
  return (
    <Tabs.Trigger className="tabs-trigger" value={value}>
      {icon}
      <span>{label}</span>
    </Tabs.Trigger>
  );
}

createRoot(root).render(
  <React.StrictMode>
    <AppErrorBoundary>
      <App />
    </AppErrorBoundary>
  </React.StrictMode>,
);
