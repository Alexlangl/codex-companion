import { providerEndpointIsChatCompletions } from "../../lib/provider-url";
import type { ProviderConfig, ProviderLaunchMode } from "../../types/domain";

export function resolveProviderLaunchMode(
  provider: ProviderConfig,
  storedMode?: ProviderLaunchMode,
  directConnectProviderIds?: readonly string[],
): ProviderLaunchMode {
  const canDirect = canDirectLaunch(provider, directConnectProviderIds);
  if (storedMode === "relay") return "relay";
  if (storedMode === "direct" && canDirect) return "direct";
  return canDirect ? "direct" : "relay";
}

export function canDirectLaunch(provider: ProviderConfig, directConnectProviderIds?: readonly string[]): boolean {
  if (directConnectProviderIds !== undefined) {
    return directConnectProviderIds.includes(provider.id);
  }
  if (providerEndpointIsChatCompletions(provider.baseUrl)) {
    return false;
  }
  const authRef = directAuthRef(provider);
  return !authRef || authRef.startsWith("env:") || authRef.startsWith("file:");
}

export function providerUsesOfficialPat(provider: ProviderConfig): boolean {
  const mode = provider.account?.authMode?.trim().toLowerCase();
  return provider.kind === "official_codex" && Boolean(mode && isOfficialPatMode(mode));
}

export function directLaunchWritesAuthJson(provider: ProviderConfig, preserveOfficialCodexAuth = false) {
  const authRef = directAuthRef(provider);
  if (!authRef.startsWith("file:")) {
    return false;
  }
  return provider.kind === "official_codex" || !preserveOfficialCodexAuth;
}

function directAuthRef(provider: ProviderConfig): string {
  const authRef = provider.authRef?.trim();
  const directAuthRef = provider.directAuthRef?.trim();
  if (provider.kind === "official_codex") {
    return authRef || directAuthRef || "";
  }
  return directAuthRef || authRef || "";
}

function isOfficialPatMode(mode: string): boolean {
  return ["pat", "personal_access_token", "personalaccesstoken", "token", "apikey", "api_key"].includes(mode);
}

export function launchModeLabel(mode: ProviderLaunchMode) {
  return mode === "direct" ? "直连" : "本地代理";
}
