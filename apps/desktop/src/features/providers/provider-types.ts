import type { ProviderKind, ProviderUsageQueryTemplate } from "../../types/domain";

export type ApiKeyKind = Extract<ProviderKind, "openai_compatible" | "relay_provider">;

export interface ApiKeyForm {
  providerDisplayName: string;
  providerName: string;
  kind: ApiKeyKind;
  baseUrl: string;
  websocketUrl: string;
  apiKey: string;
  envVar: string;
  refreshIntervalSeconds: number;
  usageQueryEnabled: boolean;
  usageQueryTemplate: ProviderUsageQueryTemplate;
  usageQueryScript: string;
  usageQueryTimeoutSeconds: number;
  usageQueryApiKey: string;
  usageQueryBaseUrl: string;
  usageQueryAccessToken: string;
  usageQueryUserId: string;
}

export interface JsonImportFile {
  name: string;
  text: string;
}

export const emptyApiKeyForm: ApiKeyForm = {
  providerDisplayName: "",
  providerName: "",
  kind: "openai_compatible",
  baseUrl: "",
  websocketUrl: "",
  apiKey: "",
  envVar: "",
  refreshIntervalSeconds: 60,
  usageQueryEnabled: false,
  usageQueryTemplate: "general",
  usageQueryScript: "",
  usageQueryTimeoutSeconds: 10,
  usageQueryApiKey: "",
  usageQueryBaseUrl: "",
  usageQueryAccessToken: "",
  usageQueryUserId: "",
};
