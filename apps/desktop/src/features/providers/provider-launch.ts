import { providerEndpointIsChatCompletions } from "../../lib/provider-url";
import type { ProviderConfig, ProviderLaunchMode } from "../../types/domain";

export function resolveProviderLaunchMode(provider: ProviderConfig, storedMode?: ProviderLaunchMode): ProviderLaunchMode {
  if (storedMode === "relay") return "relay";
  if (storedMode === "direct" && canDirectLaunch(provider)) return "direct";
  return canDirectLaunch(provider) ? "direct" : "relay";
}

export function canDirectLaunch(provider: ProviderConfig) {
  if (providerEndpointIsChatCompletions(provider.baseUrl)) {
    return false;
  }
  const authRef = provider.directAuthRef?.trim() || provider.authRef?.trim();
  return !authRef || authRef.startsWith("env:") || authRef.startsWith("file:");
}

export function directLaunchWritesAuthJson(provider: ProviderConfig, preserveOfficialCodexAuth = false) {
  const authRef = provider.directAuthRef?.trim() || provider.authRef?.trim() || "";
  if (!authRef.startsWith("file:")) {
    return false;
  }
  return provider.kind === "official_codex" || !preserveOfficialCodexAuth;
}

export function launchModeLabel(mode: ProviderLaunchMode) {
  return mode === "direct" ? "直连" : "本地代理";
}
