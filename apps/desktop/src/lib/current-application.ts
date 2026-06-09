import type { CompanionStatus, ProviderConfig, ProviderGroup, ProviderLaunchMode } from "../types/domain";
import { providerAccountTitle } from "./provider-display";

export const SINGLE_PROVIDER_GROUP_PREFIX = "single-";

export type CurrentApplication =
  | {
      kind: "group";
      id: string;
      name: string;
      description: string;
      providers: ProviderConfig[];
      group: ProviderGroup;
      launchGroupId: string;
    }
  | {
      kind: "provider";
      id: string;
      name: string;
      description: string;
      providers: ProviderConfig[];
      provider: ProviderConfig;
      launchMode: ProviderLaunchMode;
      launchGroupId?: string;
    }
  | {
      kind: "none";
      id: "";
      name: string;
      description: string;
      providers: ProviderConfig[];
    };

export function isSyntheticSingleProviderGroup(groupOrId: ProviderGroup | string | null | undefined) {
  const id = typeof groupOrId === "string" ? groupOrId : groupOrId?.id;
  return Boolean(id?.startsWith(SINGLE_PROVIDER_GROUP_PREFIX));
}

export function userVisibleGroups(status: CompanionStatus) {
  return Object.values(status.config.groups).filter((group) => !isSyntheticSingleProviderGroup(group));
}

export function currentApplication(status: CompanionStatus): CurrentApplication {
  const modelProvider = status.codex.modelProvider?.trim() || "";
  const lastDirectProviderId =
    status.config.app.lastCodexLaunchMode === "provider_direct"
      ? status.config.app.lastCodexTargetProviderId?.trim()
      : "";
  const lastDirectProvider = lastDirectProviderId ? status.config.providers[lastDirectProviderId] : undefined;
  const codexProviderId =
    modelProvider === "openai" && lastDirectProvider?.kind === "official_codex"
      ? lastDirectProviderId
      : modelProvider || lastDirectProviderId;
  if (codexProviderId && codexProviderId !== "codex-companion") {
    const provider = status.config.providers[codexProviderId];
    if (provider) {
      return providerApplication(provider, "direct");
    }
  }

  const group = status.activeGroup ?? status.config.groups[status.config.relay.activeGroupId];
  if (!group) {
    return {
      kind: "none",
      id: "",
      name: "未选择应用",
      description: "还没有可启动的分组或账号。",
      providers: [],
    };
  }

  if (isSyntheticSingleProviderGroup(group)) {
    const providerId = group.providerOrder[0] ?? group.id.slice(SINGLE_PROVIDER_GROUP_PREFIX.length);
    const provider = status.config.providers[providerId];
    if (provider) {
      return providerApplication(provider, "relay", group.id);
    }
  }

  const providers = existingGroupProviders(status, group);
  return {
    kind: "group",
    id: group.id,
    name: group.name,
    description: `${providers.length} 个账号 · ${group.fallbackEnabled ? "按优先级自动切换" : "固定使用第一个账号"}`,
    providers,
    group,
    launchGroupId: group.id,
  };
}

export function applicationProviderIds(application: CurrentApplication) {
  return new Set(application.providers.map((provider) => provider.id));
}

export function currentProviderId(application: CurrentApplication) {
  return application.kind === "provider" ? application.provider.id : null;
}

function providerApplication(provider: ProviderConfig, launchMode: ProviderLaunchMode, launchGroupId?: string): CurrentApplication {
  return {
    kind: "provider",
    id: provider.id,
    name: providerAccountTitle(provider),
    description: `${provider.name} · ${launchMode === "direct" ? "直连账号" : "本地代理单账号"}`,
    providers: [provider],
    provider,
    launchMode,
    launchGroupId,
  };
}

function existingGroupProviders(status: CompanionStatus, group: ProviderGroup) {
  const providers = group.providerOrder
    .map((id) => status.config.providers[id])
    .filter((provider): provider is ProviderConfig => Boolean(provider?.enabled));
  return group.policy === "manual" ? providers.slice(0, 1) : providers;
}
