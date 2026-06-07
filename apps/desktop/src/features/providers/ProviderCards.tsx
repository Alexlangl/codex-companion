import { Check, Play, RefreshCw, Trash2 } from "lucide-react";
import { Badge, Button, IconButton } from "../../components/ui";
import { formatTime } from "../../lib/format";
import {
  healthSummary,
  providerAccountSubtitle,
  providerAccountTitle,
  providerHealthLabel,
  providerHealthTone,
  providerTypeLabel,
  quotaInfo,
  subscriptionLabel,
  validityLabel,
  validityTone,
} from "../../lib/provider-display";
import type { CompanionStatus, ProviderConfig, ProviderLaunchMode } from "../../types/domain";
import { canDirectLaunch, launchModeLabel, resolveProviderLaunchMode } from "./provider-launch";

export function ProviderCompactItem({
  disabled,
  launchMode,
  provider,
  status,
  onLaunch,
  onLaunchModeChange,
  onRemove,
  onRefresh,
}: {
  disabled: boolean;
  launchMode?: ProviderLaunchMode;
  provider: ProviderConfig;
  status: CompanionStatus;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
}) {
  const health = status.config.health[provider.id];
  const active = status.activeProviders.some((activeProvider) => activeProvider.id === provider.id);
  const quota = quotaInfo(provider.account);
  const effectiveLaunchMode = resolveProviderLaunchMode(provider, launchMode);
  return (
    <div className={`provider-compact-item ${active ? "provider-compact-active" : ""}`}>
      <span className="compact-check" aria-label={active ? "当前账号" : "可用账号"}>
        {active ? <Check size={12} /> : null}
      </span>
      <strong>{providerAccountTitle(provider)}</strong>
      <span className={`compact-dot compact-dot-${quota.tone}`} />
      <span className="compact-quota">{quota.percentLabel}</span>
      <span className={`compact-dot compact-dot-${providerHealthTone(health?.status)}`} title={providerHealthLabel(health?.status)} />
      <span className="compact-status">{providerHealthLabel(health?.status)}</span>
      {provider.account?.validUntil ? <Badge tone={validityTone(provider.account.validUntil)}>{validityLabel(provider.account.validUntil)?.split(" · ")[0]}</Badge> : null}
      {provider.account?.subscriptionType ? <Badge tone="neutral">{provider.account.subscriptionType}</Badge> : null}
      <LaunchModeControl
        compact
        disabled={disabled}
        mode={effectiveLaunchMode}
        provider={provider}
        onChange={(mode) => void onLaunchModeChange(provider.id, mode)}
      />
      <IconButton disabled={disabled} label="刷新账号状态" onClick={() => void onRefresh(provider.id)}>
        <RefreshCw size={14} />
      </IconButton>
      <IconButton disabled={disabled} label={`启动账号：${launchModeLabel(effectiveLaunchMode)}`} onClick={() => void onLaunch(provider.id, effectiveLaunchMode)}>
        <Play size={14} />
      </IconButton>
      <IconButton disabled={disabled} label="删除账号" onClick={() => void onRemove(provider.id)}>
        <Trash2 size={14} />
      </IconButton>
    </div>
  );
}

export function ProviderCard({
  disabled,
  launchMode,
  provider,
  status,
  onLaunch,
  onLaunchModeChange,
  onRefresh,
  onRemove,
}: {
  disabled: boolean;
  launchMode?: ProviderLaunchMode;
  provider: ProviderConfig;
  status: CompanionStatus;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
}) {
  const health = status.config.health[provider.id];
  const active = status.activeProviders.some((activeProvider) => activeProvider.id === provider.id);
  const account = provider.account;
  const quota = quotaInfo(account);
  const effectiveLaunchMode = resolveProviderLaunchMode(provider, launchMode);
  const validity = validityLabel(account?.validUntil);
  const resetAt = quota.resetAt ? formatTime(quota.resetAt) : "待刷新";
  return (
    <div className={`provider-card-row ${active ? "provider-card-active" : ""}`}>
      <div className="provider-account-cell">
        <div className="provider-name-line">
          <strong>{providerAccountTitle(provider)}</strong>
          {active ? <Badge tone="ok">当前</Badge> : null}
          <Badge tone="info">{providerTypeLabel(provider)}</Badge>
          {account?.subscriptionType ? <Badge tone="accent">{account.subscriptionType}</Badge> : null}
        </div>
        <span>{providerAccountSubtitle(provider)}</span>
      </div>

      <div className="quota-compact provider-card-quota">
        <div className="quota-compact-head">
          <span>{quota.label}</span>
          <strong>{quota.percentLabel}</strong>
        </div>
        <div className="quota-compact-bar">
          <span className={`quota-compact-fill quota-compact-${quota.tone}`} style={{ width: `${quota.percent ?? 0}%` }} />
        </div>
      </div>

      <div className="provider-status-cell">
        <strong>{subscriptionLabel(provider)}</strong>
        <span>{validity ? `有效期 ${validity}` : `重置 ${resetAt}`}</span>
        <small>{healthSummary(health)}</small>
      </div>

      <div className="provider-mode-cell">
        <LaunchModeControl
          disabled={disabled}
          mode={effectiveLaunchMode}
          provider={provider}
          onChange={(mode) => void onLaunchModeChange(provider.id, mode)}
        />
        <Badge tone={providerHealthTone(health?.status)}>{providerHealthLabel(health?.status)}</Badge>
      </div>

      <div className="provider-row-actions">
        <Button disabled={disabled} onClick={() => void onLaunch(provider.id, effectiveLaunchMode)} variant="secondary">
          <Play size={15} /> 启动
        </Button>
        <IconButton disabled={disabled} label="刷新账号状态" onClick={() => void onRefresh(provider.id)}>
          <RefreshCw size={16} />
        </IconButton>
        <IconButton disabled={disabled} label="删除账号" onClick={() => void onRemove(provider.id)}>
          <Trash2 size={16} />
        </IconButton>
      </div>
      {health?.lastError ? <p className="provider-error-line">{health.lastError}</p> : null}
    </div>
  );
}

function LaunchModeControl({
  compact,
  disabled,
  mode,
  provider,
  onChange,
}: {
  compact?: boolean;
  disabled: boolean;
  mode: ProviderLaunchMode;
  provider: ProviderConfig;
  onChange: (mode: ProviderLaunchMode) => void;
}) {
  if (provider.kind === "official_codex") {
    return <Badge tone="accent">本地代理</Badge>;
  }
  const canDirect = canDirectLaunch(provider);
  return (
    <label className={`launch-mode-select ${compact ? "launch-mode-compact" : ""}`}>
      {!compact ? <span>启动方式</span> : null}
      <select
        disabled={disabled}
        onChange={(event) => onChange(event.currentTarget.value as ProviderLaunchMode)}
        title={canDirect ? "选择这个账号启动 Codex 时的请求路径" : "直连中转站需要 API Key 或 API Key 环境变量名"}
        value={mode}
      >
        <option disabled={!canDirect} value="direct">
          直连中转站
        </option>
        <option value="relay">本地代理</option>
      </select>
    </label>
  );
}
