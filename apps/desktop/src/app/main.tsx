import * as Dialog from "@radix-ui/react-dialog";
import * as Progress from "@radix-ui/react-progress";
import * as Tabs from "@radix-ui/react-tabs";
import * as Toast from "@radix-ui/react-toast";
import * as Tooltip from "@radix-ui/react-tooltip";
import "@fontsource-variable/geologica";
import { listen } from "@tauri-apps/api/event";
import {
  Boxes,
  Gauge,
  GitBranch,
  Hammer,
  History,
  LayoutDashboard,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  RadioTower,
  Settings as SettingsIcon,
  Sun,
  X,
} from "lucide-react";
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
const sidebarCollapsedStorageKey = "codex-companion.sidebar-collapsed";
const trayActionEvent = "tray-action";
const trayNavigatePrefix = "navigate:";

type PageMeta = {
  title: string;
  description: string;
};

type SidebarHeaderProps = {
  isCollapsed: boolean;
  onToggle: () => void;
};

type NavigationTabProps = {
  value: string;
  icon: React.ReactNode;
  label: string;
  showTooltip: boolean;
};

const pageMeta = {
  dashboard: { title: "总览", description: "确认当前路由、账号健康与 Codex 启动状态" },
  providers: { title: "账号", description: "管理认证材料、健康状态与启动方式" },
  groups: { title: "分组", description: "编排账号顺序与故障切换策略" },
  relay: { title: "转发", description: "监控本地 API、客户端与请求路由" },
  token: { title: "用量", description: "分析本地会话 Token 与估算成本" },
  sessions: { title: "会话", description: "查找并恢复本地 Codex 会话" },
  repair: { title: "修复", description: "预览并修复会话归属与插件状态" },
  settings: { title: "设置", description: "调整启动、更新与本地诊断选项" },
} satisfies Record<string, PageMeta>;

type AppPage = keyof typeof pageMeta;

function isAppPage(value: string): value is AppPage {
  return value in pageMeta;
}

function isTauriRuntime(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

function readSidebarCollapsed(): boolean {
  try {
    return window.localStorage.getItem(sidebarCollapsedStorageKey) === "true";
  } catch {
    return false;
  }
}

if (!root) {
  throw new Error("Missing #app root");
}

function App() {
  const { activeTab, actions, appUpdater, busy, error, progress, repairOutcome, status, toast } = useCompanionController();
  const [isSidebarCollapsed, setIsSidebarCollapsed] = React.useState(readSidebarCollapsed);
  const [pendingProviderLaunch, setPendingProviderLaunch] = React.useState<{
    mode: ProviderLaunchMode;
    provider: ProviderConfig;
  } | null>(null);
  const directLaunchDialogTitleRef = React.useRef<HTMLHeadingElement>(null);
  const activePage = isAppPage(activeTab)
    ? pageMeta[activeTab]
    : { title: "Companion", description: "Codex 本地控制平面" };
  const appShellClassName = isSidebarCollapsed ? "app-shell sidebar-collapsed" : "app-shell";
  let directLaunchDescription = "直连会更新 Codex auth.json。";
  const handleTrayAction = React.useEffectEvent((action: string): void => {
    if (action.startsWith(trayNavigatePrefix)) {
      const page = action.slice(trayNavigatePrefix.length);
      if (!isAppPage(page)) {
        return;
      }
      actions.setActiveTab(page);
      window.requestAnimationFrame(() => {
        document.querySelector<HTMLElement>("#main-content")?.focus();
      });
      return;
    }

    if (action === "launch-active-group") {
      const activeGroup = status?.activeGroup;
      if (!activeGroup) {
        actions.setToast("当前没有可启动的分组");
        return;
      }
      void actions.launchGroup(activeGroup.id);
      return;
    }

    if (action === "refresh-providers") {
      void actions.refreshAllProviders();
    }
  });

  React.useEffect(() => {
    document.title = `${activePage.title} · Codex Companion`;
  }, [activePage.title]);

  React.useEffect(() => {
    if (!isTauriRuntime()) {
      return undefined;
    }

    let isDisposed = false;
    let stopListening: (() => void) | undefined;
    void listen<string>(trayActionEvent, (event) => {
      handleTrayAction(event.payload);
    })
      .then((unlisten) => {
        if (isDisposed) {
          unlisten();
          return;
        }
        stopListening = unlisten;
      })
      .catch((unknownError: unknown) => {
        console.error("Failed to register tray action listener", unknownError);
      });

    return () => {
      isDisposed = true;
      stopListening?.();
    };
  }, []);

  React.useEffect(() => {
    try {
      window.localStorage.setItem(sidebarCollapsedStorageKey, String(isSidebarCollapsed));
    } catch {
      // The sidebar remains usable when WebView storage is unavailable.
    }
  }, [isSidebarCollapsed]);

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

  function handleSidebarToggle(): void {
    setIsSidebarCollapsed((current) => !current);
  }

  return (
    <Toast.Provider swipeDirection="right">
      <Tooltip.Provider delayDuration={180}>
        {status ? (
          <Tabs.Root className="tabs-root app-tabs-root" onValueChange={actions.setActiveTab} value={activeTab}>
            <a className="skip-link" href="#main-content">跳到主内容</a>
            <div className={appShellClassName}>
              <aside className="sidebar" id="app-sidebar">
                <SidebarHeader isCollapsed={isSidebarCollapsed} onToggle={handleSidebarToggle} />
                <nav className="sidebar-navigation" id="sidebar-navigation" aria-label="主导航">
                  <Tabs.List className="tabs-list sidebar-tabs" aria-label="Codex Companion 页面">
                    <NavigationGroup label="工作区">
                      <Tab value="dashboard" icon={<LayoutDashboard aria-hidden="true" size={16} />} label="总览" showTooltip={isSidebarCollapsed} />
                      <Tab value="providers" icon={<Boxes aria-hidden="true" size={16} />} label="账号" showTooltip={isSidebarCollapsed} />
                      <Tab value="groups" icon={<GitBranch aria-hidden="true" size={16} />} label="分组" showTooltip={isSidebarCollapsed} />
                    </NavigationGroup>
                    <NavigationGroup label="服务">
                      <Tab value="relay" icon={<RadioTower aria-hidden="true" size={16} />} label="转发" showTooltip={isSidebarCollapsed} />
                      <Tab value="token" icon={<Gauge aria-hidden="true" size={16} />} label="用量" showTooltip={isSidebarCollapsed} />
                      <Tab value="sessions" icon={<History aria-hidden="true" size={16} />} label="会话" showTooltip={isSidebarCollapsed} />
                    </NavigationGroup>
                    <NavigationGroup label="系统">
                      <Tab value="repair" icon={<Hammer aria-hidden="true" size={16} />} label="修复" showTooltip={isSidebarCollapsed} />
                      <Tab value="settings" icon={<SettingsIcon aria-hidden="true" size={16} />} label="设置" showTooltip={isSidebarCollapsed} />
                    </NavigationGroup>
                  </Tabs.List>
                </nav>
                <div className="sidebar-status" aria-live="polite">
                  <span className={`status-dot ${busy === "idle" ? "status-dot-ok" : "status-dot-busy"}`} />
                  <span>
                    <strong>{busy === "idle" ? "本地服务在线" : "正在处理"}</strong>
                    <small>LOCAL RELAY</small>
                  </span>
                </div>
              </aside>

              <main className="workspace" id="main-content" tabIndex={-1}>
                <header className="topbar">
                  <div className="topbar-title">
                    <h1>{activePage.title}</h1>
                    <p>{activePage.description}</p>
                  </div>
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
                    onImportCodexOAuth={actions.importCodexOAuth}
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
                    active={activeTab === "groups"}
                    busy={busy}
                    status={status}
                    onRequestPriorityFailback={actions.requestPriorityFailback}
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
              </main>
            </div>
          </Tabs.Root>
        ) : (
          <div className={appShellClassName}>
            <aside className="sidebar" id="app-sidebar">
              <Brand />
            </aside>
            <main className="workspace" id="main-content">
              <div className="loading-panel">正在加载 Companion 状态</div>
            </main>
          </div>
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
                  Companion 会在改写前自动备份，并在配置变化时保留用户的最新内容。开启“保留官方 Codex 登录”后，第三方 API key 仅写入对应 Provider 配置，不再修改官方 OAuth 登录。
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
      <div className="brand-icon" aria-hidden="true">CC</div>
      <div className="brand-copy">
        <strong>Companion</strong>
        <span>Codex 本地工具</span>
      </div>
    </div>
  );
}

function SidebarHeader({ isCollapsed, onToggle }: SidebarHeaderProps) {
  const actionLabel = isCollapsed ? "展开侧栏" : "收起侧栏";
  const actionIcon = isCollapsed
    ? <PanelLeftOpen aria-hidden="true" size={15} />
    : <PanelLeftClose aria-hidden="true" size={15} />;

  return (
    <div className="sidebar-header">
      <Brand />
      <Tooltip.Root>
        <Tooltip.Trigger asChild>
          <button
            aria-controls="sidebar-navigation"
            aria-expanded={!isCollapsed}
            aria-label={actionLabel}
            className="sidebar-toggle"
            onClick={onToggle}
            type="button"
          >
            {actionIcon}
          </button>
        </Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content className="sidebar-tooltip" side="right" sideOffset={8}>
            {actionLabel}
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
    </div>
  );
}

function NavigationGroup({ children, label }: { children: React.ReactNode; label: string }) {
  return (
    <>
      <span className="navigation-label" aria-hidden="true">{label}</span>
      {children}
    </>
  );
}

function Tab({ value, icon, label, showTooltip }: NavigationTabProps) {
  const trigger = (
    <Tabs.Trigger aria-label={label} className="tabs-trigger" value={value}>
      {icon}
      <span>{label}</span>
    </Tabs.Trigger>
  );

  if (!showTooltip) {
    return trigger;
  }

  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>{trigger}</Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content className="sidebar-tooltip" side="right" sideOffset={8}>
          {label}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}

createRoot(root).render(
  <React.StrictMode>
    <AppErrorBoundary>
      <App />
    </AppErrorBoundary>
  </React.StrictMode>,
);
