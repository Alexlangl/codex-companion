import { invoke } from "@tauri-apps/api/core";
import type { TokenUsageSummary } from "../types/domain";

export function getTokenUsage(codexDir?: string) {
  if (!isTauri()) {
    const params = new URLSearchParams();
    const dir = emptyToNull(codexDir);
    if (dir) params.set("codexDir", dir);
    const query = params.toString();
    return fetch(`/__codex_companion__/token-usage${query ? `?${query}` : ""}`).then(async (response) => {
      if (!response.ok) {
        const body = await response.text();
        throw new Error(body || "浏览器预览无法读取本机 Codex 会话文件");
      }
      return response.json() as Promise<TokenUsageSummary>;
    });
  }
  return invoke<TokenUsageSummary>("get_token_usage", { codexDir: emptyToNull(codexDir) });
}

function emptyToNull(value?: string) {
  return value && value.trim() ? value.trim() : null;
}

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}
