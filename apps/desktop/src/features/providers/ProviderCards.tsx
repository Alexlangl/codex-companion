import { Check, Download, Pencil, Play, RefreshCw, Trash2 } from "lucide-react";
import { Badge, IconButton } from "../../components/ui";
import { currentApplication, currentProviderId } from "../../lib/current-application";
import { formatTime } from "../../lib/format";
import {
  hasQuotaInfo,
  providerAccountSubtitle,
  providerAccountTitle,
  providerHealthLabel,
  providerHealthTone,
  providerTypeLabel,
  providerUsesAgentIdentity,
  providerUsesWebSocket,
  quotaInfo,
  validityLabel,
  validityTone,
} from "../../lib/provider-display";
import { providerEndpointIsChatCompletions } from "../../lib/provider-url";
import type { CompanionStatus, ProviderConfig, ProviderLaunchMode } from "../../types/domain";
import {
  canDirectLaunch,
  launchModeLabel,
  providerUsesOfficialPat,
  resolveProviderLaunchMode,
} from "./provider-launch";

export function ProviderCompactItem({
  disabled,
  launchMode,
  provider,
  refreshing,
  status,
  onLaunch,
  onLaunchModeChange,
  onEdit,
  onExport,
  onRemove,
  onRefresh,
}: {
  disabled: boolean;
  launchMode?: ProviderLaunchMode;
  provider: ProviderConfig;
  refreshing: boolean;
  status: CompanionStatus;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onEdit: (provider: ProviderConfig) => void;
  onExport: (provider: ProviderConfig) => void;
  onRemove: (id: string) => Promise<void>;
  onRefresh: (id: string) => Promise<void>;
}) {
  const health = status.config.health[provider.id];
  const active = currentProviderId(currentApplication(status)) === provider.id;
  const quota = quotaInfo(provider.account);
  const effectiveLaunchMode = resolveProviderLaunchMode(provider, launchMode, status.directConnectProviderIds);
  const canEdit = provider.kind !== "official_codex";
  const showPlanBadge = provider.kind === "official_codex" && Boolean(provider.account?.subscriptionType);
  const showQuota = hasQuotaInfo(quota);
  const usesAgentIdentity = providerUsesAgentIdentity(provider);
  const usesWebSocket = providerUsesWebSocket(provider);
  const providerMark = provider.kind === "official_codex" ? "C" : "API";
  const refreshDisabled = disabled || refreshing;
  return (
    <div aria-busy={refreshing} className={`provider-compact-item ${active ? "provider-compact-active" : ""}`}>
      <span className="compact-check" aria-hidden="true">
        {active ? <Check size={12} /> : providerMark}
      </span>
      <span className="sr-only">{active ? "当前账号" : "可用账号"}</span>
      <strong>{providerAccountTitle(provider)}</strong>
      {showQuota ? <span className={`compact-dot compact-dot-${quota.tone}`} /> : null}
      {showQuota ? <span className="compact-quota">{quota.percentLabel}</span> : null}
      <span className={`compact-dot compact-dot-${providerHealthTone(health?.status)}`} title={providerHealthLabel(health?.status)} />
      <span className="compact-status">{providerHealthLabel(health?.status)}</span>
      {provider.account?.validUntil ? <Badge tone={validityTone(provider.account.validUntil)}>{validityLabel(provider.account.validUntil)?.split(" · ")[0]}</Badge> : null}
      {showPlanBadge ? <Badge tone="neutral">{provider.account?.subscriptionType}</Badge> : null}
      {usesAgentIdentity ? <Badge tone="accent">Agent Identity</Badge> : null}
      {usesWebSocket ? <Badge tone="info">WebSocket</Badge> : null}
      <LaunchModeControl
        compact
        directConnectProviderIds={status.directConnectProviderIds}
        disabled={disabled}
        mode={effectiveLaunchMode}
        preserveOfficialCodexAuth={Boolean(status.config.app.preserveOfficialCodexAuth)}
        provider={provider}
        onChange={(mode) => void onLaunchModeChange(provider.id, mode)}
      />
      <IconButton disabled={refreshDisabled} label="刷新账号状态" onClick={() => void onRefresh(provider.id)}>
        <RefreshCw aria-hidden="true" className={refreshing ? "spin-icon" : undefined} size={14} />
      </IconButton>
      <IconButton disabled={disabled} label={`启动账号：${launchModeLabel(effectiveLaunchMode)}`} onClick={() => void onLaunch(provider.id, effectiveLaunchMode)}>
        <Play size={14} />
      </IconButton>
      {canEdit ? (
        <IconButton disabled={disabled} label="编辑 Provider" onClick={() => onEdit(provider)}>
          <Pencil size={14} />
        </IconButton>
      ) : null}
      <IconButton disabled={disabled} label="导出 JSON" onClick={() => onExport(provider)}>
        <Download size={14} />
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
  refreshing,
  status,
  onLaunch,
  onLaunchModeChange,
  onEdit,
  onExport,
  onRefresh,
  onRemove,
}: {
  disabled: boolean;
  launchMode?: ProviderLaunchMode;
  provider: ProviderConfig;
  refreshing: boolean;
  status: CompanionStatus;
  onLaunch: (id: string, mode?: ProviderLaunchMode) => Promise<void>;
  onLaunchModeChange: (providerId: string, mode: ProviderLaunchMode) => Promise<void>;
  onEdit: (provider: ProviderConfig) => void;
  onExport: (provider: ProviderConfig) => void;
  onRefresh: (id: string) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
}) {
  const health = status.config.health[provider.id];
  const active = currentProviderId(currentApplication(status)) === provider.id;
  const account = provider.account;
  const quota = quotaInfo(account);
  const effectiveLaunchMode = resolveProviderLaunchMode(provider, launchMode, status.directConnectProviderIds);
  const validity = validityLabel(account?.validUntil);
  const resetAt = quota.resetAt ? formatTime(quota.resetAt) : "待刷新";
  const showQuota = hasQuotaInfo(quota);
  const quotaIsBalance = showQuota && account?.quotaLabel?.includes("余额") === true;
  const showPlanBadge = provider.kind === "official_codex" && Boolean(account?.subscriptionType);
  const canEdit = provider.kind !== "official_codex";
  const lastRefreshAt = account?.lastRefreshAt ? formatTime(account.lastRefreshAt) : null;
  const usesAgentIdentity = providerUsesAgentIdentity(provider);
  const usesWebSocket = providerUsesWebSocket(provider);
  const balanceTitle = providerBalanceTitle(quotaIsBalance, quota.label);
  const balanceValue = showQuota ? quota.percentLabel : null;
  const balanceDetail = providerBalanceDetail({
    isBalance: quotaIsBalance,
    isVisible: showQuota,
    lastRefreshAt,
    planName: account?.subscriptionType,
    resetAt: quota.resetAt ? resetAt : null,
    validity,
  });
  const refreshDisabled = disabled || refreshing;
  return (
    <div aria-busy={refreshing} className={`provider-card-row ${active ? "provider-card-active" : ""}`}>
      <div className="provider-card-top">
        <div className="provider-account-cell">
          <div className="provider-name-line">
            <strong>{providerAccountTitle(provider)}</strong>
            {active ? <Badge tone="ok">当前</Badge> : null}
            <Badge tone="info">{providerTypeLabel(provider)}</Badge>
            {showPlanBadge ? <Badge tone="accent">{account?.subscriptionType}</Badge> : null}
            {usesAgentIdentity ? <Badge tone="accent">Agent Identity</Badge> : null}
            {usesWebSocket ? <Badge tone="info">WebSocket</Badge> : null}
          </div>
          <span>{providerAccountSubtitle(provider)}</span>
        </div>

        <div className="provider-row-actions">
          <IconButton disabled={disabled} label={`启动账号：${launchModeLabel(effectiveLaunchMode)}`} onClick={() => void onLaunch(provider.id, effectiveLaunchMode)}>
            <Play size={16} />
          </IconButton>
          <IconButton disabled={refreshDisabled} label="刷新账号状态" onClick={() => void onRefresh(provider.id)}>
            <RefreshCw aria-hidden="true" className={refreshing ? "spin-icon" : undefined} size={16} />
          </IconButton>
          {canEdit ? (
            <IconButton disabled={disabled} label="编辑 Provider" onClick={() => onEdit(provider)}>
              <Pencil size={16} />
            </IconButton>
          ) : null}
          <IconButton disabled={disabled} label="导出 JSON" onClick={() => onExport(provider)}>
            <Download size={16} />
          </IconButton>
          <IconButton disabled={disabled} label="删除账号" onClick={() => void onRemove(provider.id)}>
            <Trash2 size={16} />
          </IconButton>
        </div>
      </div>

      <div className={`provider-card-bottom${showQuota ? "" : " provider-card-bottom-no-quota"}`}>
        {showQuota ? (
          <div className="provider-status-cell">
            <strong>{balanceTitle}</strong>
            {balanceValue ? <span className={`provider-quota-value provider-quota-${quota.tone}`}>{balanceValue}</span> : null}
            {balanceDetail ? <small>{balanceDetail}</small> : null}
          </div>
        ) : null}

        <div className="provider-mode-cell">
          <LaunchModeControl
            directConnectProviderIds={status.directConnectProviderIds}
            disabled={disabled}
            mode={effectiveLaunchMode}
            preserveOfficialCodexAuth={Boolean(status.config.app.preserveOfficialCodexAuth)}
            provider={provider}
            onChange={(mode) => void onLaunchModeChange(provider.id, mode)}
          />
          <Badge tone={providerHealthTone(health?.status)}>{providerHealthLabel(health?.status)}</Badge>
        </div>
      </div>

      {health?.lastError ? <p className="provider-error-line">{health.lastError}</p> : null}
    </div>
  );
}

function providerBalanceTitle(isBalance: boolean, quotaLabel: string): string {
  if (isBalance) return "账户余额";
  return quotaLabel;
}

function providerBalanceDetail({
  isBalance,
  isVisible,
  lastRefreshAt,
  planName,
  resetAt,
  validity,
}: {
  isBalance: boolean;
  isVisible: boolean;
  lastRefreshAt: string | null;
  planName?: string | null;
  resetAt: string | null;
  validity: string | null;
}): string | null {
  if (isBalance) {
    return [planName, lastRefreshAt].filter(Boolean).join(" · ") || null;
  }
  if (!isVisible) return null;
  if (validity) return `有效期 ${validity}`;
  if (resetAt) return `重置 ${resetAt}`;
  return null;
}

function LaunchModeControl({
  compact,
  directConnectProviderIds,
  disabled,
  mode,
  preserveOfficialCodexAuth,
  provider,
  onChange,
}: {
  compact?: boolean;
  directConnectProviderIds?: readonly string[];
  disabled: boolean;
  mode: ProviderLaunchMode;
  preserveOfficialCodexAuth: boolean;
  provider: ProviderConfig;
  onChange: (mode: ProviderLaunchMode) => void;
}) {
  const canDirect = canDirectLaunch(provider, directConnectProviderIds);
  const usesAgentIdentity = providerUsesAgentIdentity(provider);
  const isOfficialPat = providerUsesOfficialPat(provider);
  let directTitle = "直连需要 API Key 文件、官方账号 auth 文件或环境变量";
  if (provider.kind === "official_codex") {
    if (canDirect) {
      directTitle = "官方个人访问令牌可直连；选择本地代理可由 Companion 注入账号材料";
    } else if (isOfficialPat) {
      directTitle = "PAT 凭据当前不可用；请重新导入或配置有效的个人访问令牌";
    } else if (usesAgentIdentity) {
      directTitle = "Agent Identity 固定使用 Companion 本地代理动态签名，不写入 Codex auth.json";
    } else {
      directTitle = "官方 OAuth 固定使用 Companion 本地代理；Companion 后台续期并管理 Codex auth.json 中的登录态";
    }
  } else if (providerEndpointIsChatCompletions(provider.baseUrl)) {
    directTitle =
      "该地址只接受 Chat Completions；ChatGPT / Codex 直连发送 Responses 请求，需使用 Companion 本地代理转换协议";
  } else if (canDirect && preserveOfficialCodexAuth) {
    directTitle = "直连只把 API key 写入 provider-scoped config.toml，不改现有 Codex auth.json";
  } else if (canDirect) {
    directTitle = "直连会写入 Codex 配置；API key 文件会合并进 auth.json，并可能影响官方登录态";
  }
  let relayTitle = "本地代理由 Companion 注入账号材料，不写入 Codex auth.json";
  if (provider.kind === "official_codex") {
    if (canDirect) {
      relayTitle = "本地代理由 Companion 注入官方个人访问令牌，不写入 OAuth 登录材料";
    } else if (usesAgentIdentity) {
      relayTitle = "本地代理为 Agent Identity 动态签名，不写入 Codex auth.json";
    } else {
      relayTitle = "本地代理由 Companion 后台续期 OAuth，并管理 Codex auth.json 中的官方登录态";
    }
  }
  return (
    <div aria-label="启动方式" className={`launch-mode-toggle ${compact ? "launch-mode-compact" : ""}`} role="group">
      <div className="launch-mode-options">
        <button
          aria-pressed={mode === "direct"}
          className={mode === "direct" ? "launch-mode-option launch-mode-selected" : "launch-mode-option"}
          disabled={disabled || !canDirect}
          onClick={() => onChange("direct")}
          title={directTitle}
          type="button"
        >
          直连
        </button>
        <button
          aria-pressed={mode === "relay"}
          className={mode === "relay" ? "launch-mode-option launch-mode-selected" : "launch-mode-option"}
          disabled={disabled}
          onClick={() => onChange("relay")}
          title={relayTitle}
          type="button"
        >
          代理
        </button>
      </div>
    </div>
  );
}
