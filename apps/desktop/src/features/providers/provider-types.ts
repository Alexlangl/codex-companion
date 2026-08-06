import type { ProviderKind } from "../../types/domain";

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
  usageQueryBaseUrl: "",
  usageQueryAccessToken: "",
  usageQueryUserId: "",
};
