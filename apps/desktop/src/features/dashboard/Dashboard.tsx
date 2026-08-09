import { CheckCircle2, Play, RadioTower, Router } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, Button, Panel } from "../../components/ui";
import { currentApplication, currentProviderId, launchApplication } from "../../lib/current-application";
import { compactPath, formatTime } from "../../lib/format";
import {
  hasQuotaInfo,
  providerAccountSubtitle,
  providerAccountTitle,
  providerHealthLabel,
  providerHealthTone,
  providerRunMode,
  providerTypeLabel,
  quotaInfo,
  subscriptionLabel,
} from "../../lib/provider-display";
import type { BusyState, CompanionStatus, ProviderConfig, ProviderLaunchMode } from "../../types/domain";

export function Dashboard({
  busy,
  status,
  onLaunchGroup,
  onLaunchProvider,
}: {
  busy: BusyState;
  status: CompanionStatus;
  onLaunchGroup: (id: string) => Promise<void>;
  onLaunchProvider: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
}) {
  const providers = Object.values(status.config.providers);
  const application = currentApplication(status);
  const launchTarget = launchApplication(status);
  const healthy = providers.filter((provider) => {
    const health = status.config.health[provider.id]?.status;
    return health === "healthy" || health === "unknown";
  }).length;
  const routeReady = launchTarget.kind !== "none" && launchTarget.providers.length > 0;
  const routeConnected = application.kind !== "none";
  const readinessLabel = launchReadinessLabel(routeConnected, routeReady);
  const launchTargetName = routeConnected ? application.name : `待启动：${launchTarget.name}`;
  const launchDescription = routeConnected ? application.description : status.codex.message;

  function launchCurrentApplication() {
    if (launchTarget.kind === "group") {
      void onLaunchGroup(launchTarget.launchGroupId);
    } else if (launchTarget.kind === "provider") {
      if (launchTarget.launchMode === "relay" && launchTarget.launchGroupId) {
        void onLaunchGroup(launchTarget.launchGroupId);
      } else {
        void onLaunchProvider(launchTarget.provider.id, launchTarget.launchMode);
      }
    }
  }

  return (
    <div className="dashboard-grid">
      <Panel eyebrow="当前路由" title="启动 Codex">
        <div className="launch-card">
          <div className="launch-card-main">
            <span className={`launch-readiness ${routeConnected ? "launch-readiness-ready" : ""}`}>
              <span className="status-dot" aria-hidden="true" />
              {readinessLabel}
            </span>
            <div className="launch-context">
              <strong>{launchTargetName}</strong>
              <span>{launchDescription}</span>
            </div>
          </div>
          <Button disabled={busy !== "idle" || !routeReady} onClick={launchCurrentApplication}>
            <Play aria-hidden="true" size={15} />
            启动 Codex
          </Button>
        </div>
        <div className="status-chip-grid">
          <StatusChip icon={<RadioTower aria-hidden="true" size={15} />} label="本地代理地址" value={status.relayBaseUrl} />
          <StatusChip icon={<Router aria-hidden="true" size={15} />} label="当前应用" value={application.name} />
          <StatusChip icon={<CheckCircle2 aria-hidden="true" size={15} />} label="可用账号" value={`${healthy}/${providers.length}`} />
        </div>
      </Panel>

      <Panel eyebrow="路由成员" title={launchTarget.kind === "group" ? "应用分组" : "应用账号"}>
        {launchTarget.providers.length === 0 ? (
          <p className="empty">当前应用还没有账号。去账号页面添加账号后，再到分组里编排优先级。</p>
        ) : (
          <div className="dashboard-provider-list">
            {launchTarget.providers.map((provider, index) => (
              <DashboardProviderCard
                compact={launchTarget.kind === "group"}
                healthStatus={status.config.health[provider.id]?.status}
                index={index}
                key={provider.id}
                directConnectionAvailable={status.directConnectProviderIds?.includes(provider.id)}
                provider={provider}
              />
            ))}
          </div>
        )}
      </Panel>

      <Panel eyebrow="运行环境" title="Codex 配置">
        <dl className="details-grid">
          <dt>配置文件</dt>
          <dd>{compactPath(status.codex.configPath)}</dd>
          <dt>当前 Provider</dt>
          <dd>{currentProviderLabel(status, application.kind)}</dd>
          <dt>启动方式</dt>
          <dd>{launchModeLabel(status, application.kind)}</dd>
          <dt>状态</dt>
          <dd>{status.codex.message}</dd>
        </dl>
      </Panel>
    </div>
  );
}

function launchReadinessLabel(isConnected: boolean, canLaunch: boolean): string {
  if (isConnected) return "路由就绪";
  if (canLaunch) return "Codex 未连接";
  return "等待配置";
}

function DashboardProviderCard({
  compact,
  directConnectionAvailable,
  healthStatus,
  index,
  provider,
}: {
  compact: boolean;
  directConnectionAvailable?: boolean;
  healthStatus?: string;
  index: number;
  provider: ProviderConfig;
}) {
  const quota = quotaInfo(provider.account);
  const showQuota = hasQuotaInfo(quota);
  const balanceText = showQuota && (quota.label.includes("余额") || quota.percentLabel.startsWith("$")) ? `${quota.label} ${quota.percentLabel}` : null;
  const showPlanBadge = provider.kind === "official_codex" && Boolean(provider.account?.subscriptionType);
  const accountSubtitle = providerAccountSubtitle(provider);
  const compactSubtitle = provider.kind === "official_codex" ? accountSubtitle : provider.name;
  const runMode = providerRunMode(provider, directConnectionAvailable);
  if (compact) {
    return (
      <div className="dashboard-provider-mini dashboard-provider-simple">
        <div className="dashboard-provider-simple-main">
          <strong>{index + 1}. {providerAccountTitle(provider)}</strong>
          <div className="dashboard-provider-simple-meta">
            <span>{compactSubtitle}</span>
            <span className={`dashboard-provider-health dashboard-provider-health-${providerHealthTone(healthStatus)}`}>{providerHealthLabel(healthStatus)}</span>
            {balanceText ? <span className={`dashboard-provider-balance dashboard-provider-balance-${quota.tone}`}>{balanceText}</span> : null}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="dashboard-provider-mini">
      <div className="provider-name-line">
        <strong>{index + 1}. {providerAccountTitle(provider)}</strong>
        <Badge tone="info">{providerTypeLabel(provider)}</Badge>
        {showPlanBadge ? <Badge tone="accent">{provider.account?.subscriptionType}</Badge> : null}
      </div>
      <span>{accountSubtitle}</span>
      <div className="provider-mini-foot">
        <Badge tone={runMode.includes("直连") ? "info" : "accent"}>{runMode}</Badge>
        <Badge tone={providerHealthTone(healthStatus)}>{providerHealthLabel(healthStatus)}</Badge>
        {showQuota ? <Badge tone={quota.tone}>{`${quota.label} ${quota.percentLabel}`}</Badge> : null}
        <span>{subscriptionLabel(provider)}</span>
        <span>{provider.account?.lastRefreshAt ? formatTime(provider.account.lastRefreshAt) : "待刷新"}</span>
      </div>
    </div>
  );
}

function StatusChip({ icon, label, value }: { icon: ReactNode; label: string; value: string }) {
  return (
    <div className="status-chip">
      <div className="status-chip-icon">{icon}</div>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function launchModeLabel(status: CompanionStatus, applicationKind: string) {
  if (status.codex.installed) return applicationKind === "provider" ? "单账号 / 本地代理" : "分组 / 本地代理";
  if (applicationKind === "provider") return "单 Provider 直连";
  if (status.codex.modelProvider) return "单 Provider 直连";
  return "未配置";
}

function currentProviderLabel(status: CompanionStatus, applicationKind: string) {
  const providerId = currentProviderId(currentApplication(status));
  if (providerId) return providerId;
  if (status.codex.modelProvider) return status.codex.modelProvider;
  if (
    applicationKind === "provider" &&
    status.config.app.lastCodexLaunchMode === "provider_direct" &&
    status.config.app.lastCodexTargetProviderId
  ) {
    return status.config.app.lastCodexTargetProviderId;
  }
  return "未设置";
}
