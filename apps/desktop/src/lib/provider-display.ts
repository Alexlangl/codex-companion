import type { BadgeTone } from "../components/ui";
import type {
  ProviderAccountInfo,
  ProviderConfig,
  ProviderQuotaWindow,
} from "../types/domain";
import { daysUntil, formatPercent, formatTime } from "./format";

export function providerAccountTitle(provider: ProviderConfig) {
  const account = provider.account;
  return (
    clean(account?.email) ||
    clean(account?.displayName) ||
    extractEmail(provider.name) ||
    extractEmail(provider.id) ||
    clean(provider.name) ||
    provider.id
  );
}

export function providerAccountSubtitle(provider: ProviderConfig) {
  const account = provider.account;
  const title = providerAccountTitle(provider);
  if (provider.kind === "official_codex") {
    const userId = clean(account?.userId) || clean(account?.accountId);
    const parts = [
      clean(account?.teamName) ? `Team Name: ${account?.teamName}` : null,
      userId ? `使用 workos 登录 | 用户 ID: ${shortId(userId, 30)}` : null,
      clean(account?.displayName) && clean(account?.displayName) !== title ? clean(account?.displayName) : null,
    ].filter(Boolean);
    return parts.length ? parts.join(" · ") : provider.name !== title ? provider.name : provider.id;
  }
  const displayName = clean(account?.displayName);
  const parts = [
    clean(account?.teamName) ? `Team ${account?.teamName}` : null,
    clean(account?.accountId) ? `账号 ${shortId(account?.accountId ?? "")}` : null,
    clean(account?.userId) ? `用户 ${shortId(account?.userId ?? "")}` : null,
    displayName && displayName !== title ? displayName : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : provider.name !== title ? provider.name : provider.id;
}

export function providerSecondaryLine(provider: ProviderConfig) {
  const account = provider.account;
  return [
    account?.subscriptionStatus,
    validityLabel(account?.validUntil),
    account?.lastRefreshAt ? `上次刷新 ${formatTime(account.lastRefreshAt)}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
}

export function providerRunMode(provider: ProviderConfig) {
  if (provider.kind === "official_codex") return "可直连";
  const directAuthRef = provider.directAuthRef?.trim();
  const authRef = provider.authRef?.trim();
  const directRef = directAuthRef || authRef;
  if (!directRef || directRef.startsWith("env:") || directRef.startsWith("file:")) return "可直连";
  return "可本地代理";
}

export function providerTypeLabel(provider: ProviderConfig) {
  if (provider.kind === "official_codex") return "Codex 官方账号";
  return "API Key";
}

export function providerHealthLabel(status?: string) {
  switch (status) {
    case "healthy":
      return "健康";
    case "degraded":
      return "异常";
    case "cooldown":
      return "冷却中";
    case "quota_exhausted":
      return "额度耗尽";
    case "rate_limited":
      return "限流";
    case "auth_failed":
      return "认证失败";
    case "model_missing":
      return "模型缺失";
    case "offline":
      return "离线";
    case "unknown":
    default:
      return "未刷新";
  }
}

export function providerHealthTone(status?: string): BadgeTone {
  switch (status) {
    case "healthy":
      return "ok";
    case "degraded":
    case "cooldown":
    case "rate_limited":
      return "warn";
    case "quota_exhausted":
    case "auth_failed":
    case "model_missing":
    case "offline":
      return "danger";
    case "unknown":
    default:
      return "neutral";
  }
}

export function quotaInfo(account?: ProviderAccountInfo | null): {
  label: string;
  percent: number | null;
  percentLabel: string;
  resetAt: string | null;
  tone: BadgeTone;
} {
  const window = primaryQuotaWindow(account?.quotaWindows);
  const percent = window?.remainingPercent ?? account?.quotaPercent ?? null;
  const usage = formatApiUsage(account?.usageAvailable, account?.usageTotal, account?.usageUsed, account?.quotaLabel);
  const label = window ? `${window.label} 剩余额度` : account?.quotaLabel || usage || "额度待刷新";
  return {
    label,
    percent,
    percentLabel: formatPercent(percent) || usage || (account?.quotaLabel ? "已记录" : "待刷新"),
    resetAt: window?.resetAt ?? account?.quotaResetAt ?? null,
    tone: quotaTone(percent),
  };
}

export function hasQuotaInfo(quota: { label: string; percentLabel: string }) {
  return quota.label !== "额度待刷新" || quota.percentLabel !== "待刷新";
}

export function validityLabel(value?: string | null) {
  if (!value) return null;
  const days = daysUntil(value);
  return days ? `${days} · ${formatTime(value)}` : formatTime(value);
}

export function validityTone(value?: string | null): BadgeTone {
  if (!value) return "neutral";
  const days = daysUntil(value);
  if (days === "已过期") return "danger";
  const numericDays = days ? Number.parseInt(days, 10) : Number.NaN;
  return Number.isFinite(numericDays) && numericDays <= 7 ? "warn" : "ok";
}

export function subscriptionLabel(provider: ProviderConfig) {
  if (provider.kind === "official_codex") {
    return [provider.account?.subscriptionType, provider.account?.subscriptionStatus].filter(Boolean).join(" / ") || "待刷新订阅";
  }
  return ["API Key", provider.account?.subscriptionStatus].filter(Boolean).join(" / ");
}

export function quotaTone(value?: number | null): BadgeTone {
  if (value === undefined || value === null || Number.isNaN(value)) return "neutral";
  if (value <= 15) return "danger";
  if (value <= 35) return "warn";
  return "ok";
}

export function shortId(value: string, visible = 8) {
  return value.length > visible + 3 ? `${value.slice(0, visible)}...` : value;
}

function primaryQuotaWindow(windows?: ProviderQuotaWindow[] | null) {
  if (!windows?.length) return null;
  return windows.reduce((lowest, window) => (window.remainingPercent < lowest.remainingPercent ? window : lowest), windows[0]);
}

function formatApiUsage(available?: number | null, total?: number | null, used?: number | null, label?: string | null) {
  if (available === undefined || available === null) return null;
  if (label?.includes("余额")) return `$${compactNumber(available)}`;
  if (total !== undefined && total !== null && total > 0) {
    const usedText = used !== undefined && used !== null ? ` · 已用 ${compactNumber(used)}` : "";
    return `${compactNumber(available)} / ${compactNumber(total)}${usedText}`;
  }
  return `剩余 ${compactNumber(available)}`;
}

function compactNumber(value: number) {
  return Number.isInteger(value) ? `${value}` : value.toFixed(2);
}

function clean(value?: string | null) {
  const trimmed = value?.trim();
  return trimmed || null;
}

function extractEmail(value?: string | null) {
  return value?.match(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/i)?.[0] ?? null;
}
