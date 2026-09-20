import { invoke } from "@tauri-apps/api/core";
import type { PricingSettings, PricingSettingsSnapshot } from "../types/domain";

export const pricingPreview = !("__TAURI_INTERNALS__" in window);
const PREVIEW_KEY = "companion-pricing-preview-v1";
const previewDefaults: PricingSettingsSnapshot = {
  builtinModels: [{ model: "gpt-5.4", inputPerMillion: "2.50", cachedInputPerMillion: "0.25", cacheWriteInputPerMillion: "2.50", outputPerMillion: "15.00", aliases: [] }],
  overrides: { models: [], providerMultipliers: {} },
  pricingAsOf: "2026-08-09",
};

export function getPricingSettings(): Promise<PricingSettingsSnapshot> {
  if (pricingPreview) {
    const stored = localStorage.getItem(PREVIEW_KEY);
    const overrides: PricingSettings = stored ? JSON.parse(stored) : previewDefaults.overrides;
    return Promise.resolve({ ...previewDefaults, overrides });
  }
  return invoke<PricingSettingsSnapshot>("get_pricing_settings");
}

export function savePricingSettings(input: PricingSettings): Promise<PricingSettingsSnapshot> {
  if (pricingPreview) {
    localStorage.setItem(PREVIEW_KEY, JSON.stringify(input));
    return Promise.resolve({ ...previewDefaults, overrides: input });
  }
  return invoke<PricingSettingsSnapshot>("save_pricing_settings", { input });
}
