import type { ProviderConfig, ProviderLaunchMode } from "../../types/domain";

export function resolveProviderLaunchMode(provider: ProviderConfig, storedMode?: ProviderLaunchMode): ProviderLaunchMode {
  if (storedMode === "relay") return "relay";
  if (storedMode === "direct" && canDirectLaunch(provider)) return "direct";
  return canDirectLaunch(provider) ? "direct" : "relay";
}

export function canDirectLaunch(provider: ProviderConfig) {
  const authRef = provider.directAuthRef?.trim() || provider.authRef?.trim();
  return !authRef || authRef.startsWith("env:") || authRef.startsWith("file:");
}

export function launchModeLabel(mode: ProviderLaunchMode) {
  return mode === "direct" ? "直连" : "本地代理";
}
