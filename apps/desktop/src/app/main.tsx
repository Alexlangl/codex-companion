import * as Dialog from "@radix-ui/react-dialog";
import * as Progress from "@radix-ui/react-progress";
import * as Tabs from "@radix-ui/react-tabs";
import * as Toast from "@radix-ui/react-toast";
import * as Tooltip from "@radix-ui/react-tooltip";
import { Boxes, Gauge, GitBranch, Hammer, LayoutDashboard, Moon, RadioTower, Settings as SettingsIcon, Sun, X } from "lucide-react";
import React from "react";
import { createRoot } from "react-dom/client";
import { Badge, Button, IconButton } from "../components/ui";
import { Dashboard } from "../features/dashboard/Dashboard";
import { Groups } from "../features/groups/Groups";
import { Providers } from "../features/providers/Providers";
import { canDirectLaunch, directLaunchWritesAuthJson } from "../features/providers/provider-launch";
import { Relay } from "../features/relay/Relay";
import { Repair } from "../features/repair/Repair";
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

const root = document.querySelector<HTMLDivElement>("#app");

if (!root) {
  throw new Error("Missing #app root");
}

function App() {
  const { activeTab, actions, busy, error, progress, repairOutcome, status, toast } = useCompanionController();
  const [pendingProviderLaunch, setPendingProviderLaunch] = React.useState<{
    mode: ProviderLaunchMode;
    provider: ProviderConfig;
  } | null>(null);

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
    if (willDirect && directLaunchWritesAuthJson(provider)) {
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
                <div className="brand" aria-label="Codex Companion">
                  <div className="brand-icon">CC</div>
                </div>
                <Tabs.List className="tabs-list sidebar-tabs" aria-label="Codex Companion">
                  <Tab value="dashboard" icon={<LayoutDashboard size={16} />} label="总览" />
                  <Tab value="providers" icon={<Boxes size={16} />} label="账号" />
                  <Tab value="groups" icon={<GitBranch size={16} />} label="分组" />
                  <Tab value="relay" icon={<RadioTower size={16} />} label="转发" />
                  <Tab value="token" icon={<Gauge size={16} />} label="用量" />
                  <Tab value="repair" icon={<Hammer size={16} />} label="修复" />
                  <Tab value="settings" icon={<SettingsIcon size={16} />} label="设置" />
                </Tabs.List>
              </aside>

              <section className="workspace">
                <header className="topbar">
                  <div>
                    <h1>Codex Companion</h1>
                    <p>本地代理、账号分组、失败切换与 Codex 状态修复。</p>
                  </div>
                  <div className="topbar-actions">
                    <IconButton
                      label={status.config.app.theme === "dark" ? "切换到亮色主题" : "切换到暗色主题"}
                      onClick={() => void actions.toggleTheme()}
                    >
                      {status.config.app.theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
                    </IconButton>
                    <Badge tone={busy === "idle" ? "ok" : "warn"}>{busy === "idle" ? "就绪" : "处理中"}</Badge>
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
                  <Relay status={status} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="token">
                  <TokenStats active={activeTab === "token"} status={status} onLoad={actions.loadTokenUsage} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="repair">
                  <Repair outcome={repairOutcome} status={status} onRepair={actions.repair} />
                </Tabs.Content>
                <Tabs.Content className="tabs-content" value="settings">
                  <Settings
                    busy={busy}
                    status={status}
                    onInstall={actions.install}
                    onUninstall={actions.uninstall}
                    onPreserveOfficialCodexAuth={actions.changePreserveOfficialCodexAuth}
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
              <div className="brand" aria-label="Codex Companion">
                <div className="brand-icon">CC</div>
              </div>
            </aside>
            <section className="workspace">
              <div className="loading-panel">正在加载 Companion 状态</div>
            </section>
          </main>
        )}
        <Dialog.Root open={Boolean(pendingProviderLaunch)} onOpenChange={(open) => !open && setPendingProviderLaunch(null)}>
          <Dialog.Portal>
            <Dialog.Overlay className="dialog-overlay" />
            <Dialog.Content className="dialog-content direct-launch-confirm-dialog">
              <div className="dialog-header">
                <div>
                  <Dialog.Title className="dialog-title">确认直连写入</Dialog.Title>
                  <Dialog.Description className="dialog-description">
                    {pendingProviderLaunch
                      ? `${providerAccountTitle(pendingProviderLaunch.provider)} 将把账号材料合并写入 Codex auth.json。`
                      : "直连会更新 Codex auth.json。"}
                  </Dialog.Description>
                </div>
                <Dialog.Close className="icon-button" aria-label="关闭">
                  <X size={16} />
                </Dialog.Close>
              </div>
              <div className="warning-box">
                <strong>这会修改本机 Codex 登录材料</strong>
                <p>
                  Companion 会先备份并记录 ownership marker；如果已开启官方登录保护，后端会阻止可能覆盖官方 OAuth 的第三方 API key 直连。
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
    <App />
  </React.StrictMode>,
);
