import { CheckCircle2, Play, RadioTower, Router } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, Button, Panel } from "../../components/ui";
import { currentApplication } from "../../lib/current-application";
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
  const healthy = providers.filter((provider) => {
    const health = status.config.health[provider.id]?.status;
    return health === "healthy" || health === "unknown";
  }).length;

  function launchCurrentApplication() {
    if (application.kind === "group") {
      void onLaunchGroup(application.launchGroupId);
    } else if (application.kind === "provider") {
      if (application.launchMode === "relay" && application.launchGroupId) {
        void onLaunchGroup(application.launchGroupId);
      } else {
        void onLaunchProvider(application.provider.id, application.launchMode);
      }
    }
  }

  return (
    <div className="dashboard-grid">
      <Panel eyebrow="启动" title="启动 Codex">
        <div className="launch-card">
          <div>
            <strong>{application.name}</strong>
            <span>{application.description}</span>
          </div>
          <Button disabled={busy !== "idle" || application.kind === "none" || application.providers.length === 0} iconOnly label="启动当前应用" onClick={launchCurrentApplication}>
            <Play size={16} />
          </Button>
        </div>
        <div className="status-chip-grid">
          <StatusChip icon={<RadioTower size={15} />} label="本地代理地址" value={status.relayBaseUrl} />
          <StatusChip icon={<Router size={15} />} label="当前应用" value={application.name} />
          <StatusChip icon={<CheckCircle2 size={15} />} label="可用账号" value={`${healthy}/${providers.length}`} />
        </div>
      </Panel>

      <Panel eyebrow="当前应用" title={application.kind === "group" ? "应用分组" : "应用账号"}>
        {application.providers.length === 0 ? (
          <p className="empty">当前应用还没有账号。去账号页面添加账号后，再到分组里编排优先级。</p>
        ) : (
          <div className="dashboard-provider-list">
            {application.providers.map((provider, index) => (
              <DashboardProviderCard
                compact={application.kind === "group"}
                healthStatus={status.config.health[provider.id]?.status}
                index={index}
                key={provider.id}
                provider={provider}
              />
            ))}
          </div>
        )}
      </Panel>

      <Panel eyebrow="Codex" title="启动配置">
        <dl className="details-grid">
          <dt>配置文件</dt>
          <dd>{compactPath(status.codex.configPath)}</dd>
          <dt>当前 Provider</dt>
          <dd>{status.codex.modelProvider ?? "未设置"}</dd>
          <dt>启动方式</dt>
          <dd>{launchModeLabel(status, application.kind)}</dd>
          <dt>状态</dt>
          <dd>{status.codex.message}</dd>
        </dl>
      </Panel>
    </div>
  );
}

function DashboardProviderCard({
  compact,
  healthStatus,
  index,
  provider,
}: {
  compact: boolean;
  healthStatus?: string;
  index: number;
  provider: ProviderConfig;
}) {
  const quota = quotaInfo(provider.account);
  const showQuota = hasQuotaInfo(quota);
  const balanceText = showQuota && (quota.label.includes("余额") || quota.percentLabel.startsWith("$")) ? `${quota.label} ${quota.percentLabel}` : null;
  const showPlanBadge = provider.kind === "official_codex" && Boolean(provider.account?.subscriptionType);
  if (compact) {
    return (
      <div className="dashboard-provider-mini dashboard-provider-simple">
        <div className="dashboard-provider-simple-main">
          <strong>{index + 1}. {providerAccountTitle(provider)}</strong>
          <div className="dashboard-provider-simple-meta">
            <span>{provider.name}</span>
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
      <span>{providerAccountSubtitle(provider)}</span>
      <div className="provider-mini-foot">
        <Badge tone={providerRunMode(provider).includes("直连") ? "info" : "accent"}>{providerRunMode(provider)}</Badge>
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
  if (status.codex.modelProvider) return "单 Provider 直连";
  return "未配置";
}
