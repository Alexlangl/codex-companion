import { invoke } from "@tauri-apps/api/core";
import type { TokenUsageQuery, TokenUsageSummary, TokenUsageSyncStatus } from "../types/domain";

export function getTokenUsage(codexDir?: string, query: TokenUsageQuery = {}) {
  if (!isTauri()) {
    const params = new URLSearchParams();
    const dir = emptyToNull(codexDir);
    if (dir) params.set("codexDir", dir);
    if (query.startDate) params.set("startDate", query.startDate);
    if (query.endDate) params.set("endDate", query.endDate);
    if (query.providerId) params.set("providerId", query.providerId);
    if (query.model) params.set("model", query.model);
    if (query.rebuild) params.set("rebuild", "true");
    const queryString = params.toString();
    return fetch(`/__codex_companion__/token-usage${queryString ? `?${queryString}` : ""}`).then(async (response) => {
      if (!response.ok) {
        const body = await response.text();
        throw new Error(body || "浏览器预览无法读取本机 Codex 会话文件");
      }
      return response.json() as Promise<TokenUsageSummary>;
    });
  }
  return invoke<TokenUsageSummary>("get_token_usage", {
    codexDir: emptyToNull(codexDir),
    startDate: query.startDate ?? null,
    endDate: query.endDate ?? null,
    providerId: query.providerId ?? null,
    model: query.model ?? null,
    rebuild: query.rebuild ?? false,
  });
}

export function getTokenUsageSyncStatus() {
  if (!isTauri()) {
    return Promise.resolve<TokenUsageSyncStatus>({
      active: false,
      scannedFiles: 0,
      totalFiles: 0,
      deferredFiles: 0,
      suspectedDuplicates: 0,
      phase: "complete",
      startedAt: null,
      finishedAt: null,
    });
  }
  return invoke<TokenUsageSyncStatus>("get_token_usage_sync_status");
}

function emptyToNull(value?: string) {
  return value && value.trim() ? value.trim() : null;
}

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}
