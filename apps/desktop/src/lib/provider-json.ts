import type { ProviderQuotaWindow } from "../types/domain";

export function isApiKeyJson(value: unknown) {
  const authMode = findString(value, ["auth_mode", "authMode"])?.toLowerCase();
  return Boolean((authMode === "apikey" || authMode === "api_key") && findString(value, ["OPENAI_API_KEY", "openai_api_key", "openaiApiKey", "api_key", "apiKey"]));
}

export function providerNameFromBaseUrl(baseUrl: string) {
  try {
    return new URL(baseUrl).hostname.replace(/^www\./, "") || "OpenAI API Key";
  } catch {
    return "OpenAI API Key";
  }
}

export function findString(value: unknown, keys: string[]): string | null {
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findString(item, keys);
      if (found) return found;
    }
    return null;
  }
  const record = value as Record<string, unknown>;
  for (const key of keys) {
    const item = record[key];
    if (typeof item === "string" && item.trim()) return item.trim();
  }
  for (const item of Object.values(record)) {
    const found = findString(item, keys);
    if (found) return found;
  }
  return null;
}

export function findNumber(value: unknown, keys: string[]): number | null {
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findNumber(item, keys);
      if (found !== null) return found;
    }
    return null;
  }
  const record = value as Record<string, unknown>;
  for (const key of keys) {
    const item = record[key];
    const number = normalizeNumber(item);
    if (number !== null) return number;
  }
  for (const item of Object.values(record)) {
    const found = findNumber(item, keys);
    if (found !== null) return found;
  }
  return null;
}

export function extractQuotaWindows(value: unknown): ProviderQuotaWindow[] {
  return [
    extractQuotaWindow(
      value,
      "5h",
      ["hourly_percentage", "hourlyPercentage", "primary_window_remaining_percent", "primaryWindowRemainingPercent"],
      ["hourly_reset_time", "hourlyResetTime", "primary_window_reset_at", "primaryWindowResetAt"],
      ["hourly_window_minutes", "hourlyWindowMinutes", "primary_window_minutes", "primaryWindowMinutes"],
    ),
    extractQuotaWindow(
      value,
      "Week",
      ["weekly_percentage", "weeklyPercentage", "secondary_window_remaining_percent", "secondaryWindowRemainingPercent"],
      ["weekly_reset_time", "weeklyResetTime", "secondary_window_reset_at", "secondaryWindowResetAt"],
      ["weekly_window_minutes", "weeklyWindowMinutes", "secondary_window_minutes", "secondaryWindowMinutes"],
    ),
    extractQuotaWindow(
      value,
      "Code Review",
      ["code_review_percentage", "codeReviewPercentage", "code_review_remaining_percent", "codeReviewRemainingPercent"],
      ["code_review_reset_time", "codeReviewResetTime"],
      ["code_review_window_minutes", "codeReviewWindowMinutes"],
    ),
  ].filter((window): window is ProviderQuotaWindow => Boolean(window));
}

export function lowestQuotaPercent(windows: ProviderQuotaWindow[]) {
  if (windows.length === 0) return null;
  return windows.reduce((lowest, window) => (window.remainingPercent < lowest ? window.remainingPercent : lowest), windows[0].remainingPercent);
}

function extractQuotaWindow(
  value: unknown,
  label: string,
  percentKeys: string[],
  resetKeys: string[],
  windowKeys: string[],
): ProviderQuotaWindow | null {
  const remainingPercent = findNumber(value, percentKeys);
  if (remainingPercent === null) return null;
  return {
    label,
    remainingPercent,
    resetAt: findString(value, resetKeys),
    windowMinutes: findNumber(value, windowKeys),
  };
}

function normalizeNumber(value: unknown): number | null {
  const parsed =
    typeof value === "number"
      ? value
      : typeof value === "string"
        ? Number(value.trim().replace(/%$/, ""))
        : Number.NaN;
  if (Number.isNaN(parsed)) return null;
  const percent = parsed >= 0 && parsed <= 1 ? parsed * 100 : parsed;
  return Math.min(100, Math.max(0, percent));
}
