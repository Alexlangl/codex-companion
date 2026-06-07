import { CheckCircle2, Play, RadioTower, Router } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, Button, Panel } from "../../components/ui";
import { compactPath, formatTime } from "../../lib/format";
import {
  providerAccountSubtitle,
  providerAccountTitle,
  providerHealthLabel,
  providerHealthTone,
  providerRunMode,
  providerTypeLabel,
  quotaInfo,
  subscriptionLabel,
} from "../../lib/provider-display";
import type { BusyState, CompanionStatus, ProviderConfig } from "../../types/domain";

export function Dashboard({
  busy,
  status,
  onLaunchGroup,
}: {
  busy: BusyState;
  status: CompanionStatus;
  onLaunchGroup: (id: string) => Promise<void>;
}) {
  const providers = Object.values(status.config.providers);
  const healthy = providers.filter((provider) => {
    const health = status.config.health[provider.id]?.status;
    return health === "healthy" || health === "unknown";
  }).length;

  return (
    <div className="dashboard-grid">
      <Panel eyebrow="启动" title="启动 Codex">
        <div className="launch-card">
          <div>
            <strong>{status.activeGroup?.name ?? "Default"}</strong>
            <span>{status.activeProviders.length} 个账号按优先级参与失败切换</span>
          </div>
          <Button disabled={busy !== "idle" || !status.activeGroup} onClick={() => status.activeGroup && void onLaunchGroup(status.activeGroup.id)}>
            <Play size={15} /> 启动当前分组
          </Button>
        </div>
        <div className="status-chip-grid">
          <StatusChip icon={<RadioTower size={15} />} label="本地转发地址" value={status.relayBaseUrl} />
          <StatusChip icon={<Router size={15} />} label="当前分组" value={status.activeGroup?.name ?? "Default"} />
          <StatusChip icon={<CheckCircle2 size={15} />} label="可用账号" value={`${healthy}/${providers.length}`} />
        </div>
      </Panel>

      <Panel eyebrow="当前路由" title="当前账号">
        {status.activeProviders.length === 0 ? (
          <p className="empty">当前分组还没有账号。去账号页面添加账号后，再到分组里编排优先级。</p>
        ) : (
          <div className="dashboard-provider-list">
            {status.activeProviders.map((provider, index) => (
              <DashboardProviderCard
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
          <dd>{launchModeLabel(status)}</dd>
          <dt>状态</dt>
          <dd>{status.codex.message}</dd>
        </dl>
      </Panel>
    </div>
  );
}

function DashboardProviderCard({
  healthStatus,
  index,
  provider,
}: {
  healthStatus?: string;
  index: number;
  provider: ProviderConfig;
}) {
  const quota = quotaInfo(provider.account);
  return (
    <div className="dashboard-provider-mini">
      <div className="provider-name-line">
        <strong>{index + 1}. {providerAccountTitle(provider)}</strong>
        <Badge tone="info">{providerTypeLabel(provider)}</Badge>
        {provider.account?.subscriptionType ? <Badge tone="accent">{provider.account.subscriptionType}</Badge> : null}
      </div>
      <span>{providerAccountSubtitle(provider)}</span>
      <div className="quota-compact">
        <div className="quota-compact-head">
          <span>{quota.label}</span>
          <strong>{quota.percentLabel}</strong>
        </div>
        <div className="quota-compact-bar">
          <span className={`quota-compact-fill quota-compact-${quota.tone}`} style={{ width: `${quota.percent ?? 0}%` }} />
        </div>
      </div>
      <div className="provider-mini-foot">
        <Badge tone={providerRunMode(provider).includes("直连") ? "info" : "accent"}>{providerRunMode(provider)}</Badge>
        <Badge tone={providerHealthTone(healthStatus)}>{providerHealthLabel(healthStatus)}</Badge>
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

function launchModeLabel(status: CompanionStatus) {
  if (status.codex.installed) return "分组 / Companion 转发";
  if (status.codex.modelProvider) return "单 Provider 直连";
  return "未配置";
}
