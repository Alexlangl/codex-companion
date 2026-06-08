import type { ProviderKind, ProviderQuotaWindow } from "../types/domain";

export function providerKindLabel(kind: ProviderKind) {
  switch (kind) {
    case "official_codex":
      return "Codex 官方账号";
    case "openai_compatible":
      return "OpenAI 兼容";
    case "relay_provider":
      return "中转站";
  }
}

export function statusLabel(status: string) {
  const labels: Record<string, string> = {
    unknown: "未刷新",
    healthy: "健康",
    degraded: "异常",
    cooldown: "冷却中",
    quota_exhausted: "额度耗尽",
    rate_limited: "限流",
    auth_failed: "认证失败",
    model_missing: "模型缺失",
    offline: "离线",
  };
  return labels[status] ?? status;
}

export function compactPath(path: string) {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

export function formatTime(value?: string | null) {
  if (!value) return "从未";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

export function formatPercent(value?: number | null) {
  if (value === undefined || value === null || Number.isNaN(value)) return null;
  return `${Math.round(value)}%`;
}

export function formatQuotaWindows(windows?: ProviderQuotaWindow[] | null) {
  if (!windows?.length) return null;
  return windows
    .map((window) => `${window.label} ${formatPercent(window.remainingPercent) ?? "未知"}`)
    .join(" / ");
}

export function formatQuotaReset(windows?: ProviderQuotaWindow[] | null) {
  if (!windows?.length) return null;
  return windows
    .filter((window) => window.resetAt)
    .map((window) => `${window.label} ${formatTime(window.resetAt)}`)
    .join(" / ");
}

export function daysUntil(value?: string | null) {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;
  const days = Math.ceil((date.getTime() - Date.now()) / 86_400_000);
  return days >= 0 ? `${days}天` : "已过期";
}

export function formatTokens(value: number) {
  if (value >= 1_000_000) return `${trimFixed(value / 1_000_000)}M`;
  if (value >= 1_000) return `${trimFixed(value / 1_000)}K`;
  return `${value}`;
}

function trimFixed(value: number) {
  return value.toFixed(value >= 10 ? 1 : 2).replace(/\\.0+$/, "");
}
