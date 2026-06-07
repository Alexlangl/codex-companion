import type { ProviderKind } from "../../types/domain";

export type ApiKeyKind = Extract<ProviderKind, "openai_compatible" | "relay_provider">;

export interface ApiKeyForm {
  providerName: string;
  kind: ApiKeyKind;
  baseUrl: string;
  apiKey: string;
  envVar: string;
  refreshIntervalSeconds: number;
}

export interface JsonImportFile {
  name: string;
  text: string;
}

export const emptyApiKeyForm: ApiKeyForm = {
  providerName: "",
  kind: "openai_compatible",
  baseUrl: "",
  apiKey: "",
  envVar: "",
  refreshIntervalSeconds: 60,
};
