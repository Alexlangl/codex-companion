use codex_companion_core::{CompanionError, Result, TokenCostBreakdown};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const PRICING_AS_OF: &str = "2026-07-10";
const TOKENS_PER_MILLION: u64 = 1_000_000;

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub model: String,
    input_per_million: Decimal,
    cached_input_per_million: Decimal,
    output_per_million: Decimal,
}

#[derive(Debug, Clone, Default)]
pub struct CostBreakdown {
    pub fresh_input_usd: Decimal,
    pub cached_input_usd: Decimal,
    pub output_usd: Decimal,
}

impl CostBreakdown {
    pub fn total_usd(&self) -> Decimal {
        self.fresh_input_usd + self.cached_input_usd + self.output_usd
    }

    pub fn add_assign(&mut self, other: &Self) {
        self.fresh_input_usd += other.fresh_input_usd;
        self.cached_input_usd += other.cached_input_usd;
        self.output_usd += other.output_usd;
    }

    pub fn to_api(&self) -> TokenCostBreakdown {
        TokenCostBreakdown {
            fresh_input_usd: format_usd(self.fresh_input_usd),
            cached_input_usd: format_usd(self.cached_input_usd),
            output_usd: format_usd(self.output_usd),
            total_usd: format_usd(self.total_usd()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PricingCatalog {
    models: BTreeMap<String, ModelPricing>,
    provider_multipliers: BTreeMap<String, Decimal>,
    pub override_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PricingOverride {
    #[serde(default)]
    models: Vec<ModelPricingOverride>,
    #[serde(default)]
    provider_multipliers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPricingOverride {
    model: String,
    input_per_million: String,
    cached_input_per_million: String,
    output_per_million: String,
    #[serde(default)]
    aliases: Vec<String>,
}

impl PricingCatalog {
    pub fn builtin() -> Self {
        let mut catalog = Self {
            models: BTreeMap::new(),
            provider_multipliers: BTreeMap::new(),
            override_path: None,
        };
        for (model, input, cached_input, output) in [
            ("gpt-5.6-sol", "5.00", "0.50", "30.00"),
            ("gpt-5.6-terra", "2.50", "0.25", "15.00"),
            ("gpt-5.6-luna", "1.00", "0.10", "6.00"),
            ("gpt-5.5", "5.00", "0.50", "30.00"),
            ("gpt-5.4", "2.50", "0.25", "15.00"),
            ("gpt-5.4-mini", "0.75", "0.075", "4.50"),
            ("gpt-5.4-nano", "0.20", "0.02", "1.25"),
            ("gpt-5.3-codex", "1.75", "0.175", "14.00"),
            ("gpt-5.3-chat-latest", "5.00", "0.50", "30.00"),
        ] {
            catalog.insert(
                model,
                decimal(input),
                decimal(cached_input),
                decimal(output),
                &[],
            );
        }
        catalog
    }

    pub fn load_override(mut self, path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(self);
        }
        let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
        let value = serde_json::from_str::<PricingOverride>(&text)
            .map_err(|source| CompanionError::json(path, source))?;
        for model in value.models {
            let input = parse_nonnegative_decimal(
                path,
                &model.model,
                "inputPerMillion",
                &model.input_per_million,
            )?;
            let cached_input = parse_nonnegative_decimal(
                path,
                &model.model,
                "cachedInputPerMillion",
                &model.cached_input_per_million,
            )?;
            let output = parse_nonnegative_decimal(
                path,
                &model.model,
                "outputPerMillion",
                &model.output_per_million,
            )?;
            self.insert(&model.model, input, cached_input, output, &model.aliases);
        }
        for (provider_id, raw_multiplier) in value.provider_multipliers {
            let multiplier = Decimal::from_str(raw_multiplier.trim()).map_err(|source| {
                CompanionError::InvalidConfig(format!(
                    "invalid pricing multiplier for provider {provider_id} in {}: {source}",
                    path.display()
                ))
            })?;
            if multiplier <= Decimal::ZERO {
                return Err(CompanionError::InvalidConfig(format!(
                    "pricing multiplier for provider {provider_id} in {} must be greater than zero",
                    path.display()
                )));
            }
            self.provider_multipliers
                .insert(provider_id.trim().to_ascii_lowercase(), multiplier);
        }
        self.override_path = Some(path.to_path_buf());
        Ok(self)
    }

    pub fn find(&self, raw_model: &str) -> Option<&ModelPricing> {
        model_pricing_candidates(raw_model)
            .into_iter()
            .find_map(|candidate| self.models.get(&candidate))
    }

    pub fn estimate(
        &self,
        raw_model: &str,
        provider_id: Option<&str>,
        fresh_input_tokens: u64,
        cached_input_tokens: u64,
        output_tokens: u64,
    ) -> Option<(&ModelPricing, CostBreakdown)> {
        let pricing = self.find(raw_model)?;
        let multiplier = provider_id
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .and_then(|provider_id| self.provider_multipliers.get(&provider_id).copied())
            .unwrap_or(Decimal::ONE);
        let per_million = Decimal::from(TOKENS_PER_MILLION);
        let cost = CostBreakdown {
            fresh_input_usd: Decimal::from(fresh_input_tokens) * pricing.input_per_million
                / per_million
                * multiplier,
            cached_input_usd: Decimal::from(cached_input_tokens) * pricing.cached_input_per_million
                / per_million
                * multiplier,
            output_usd: Decimal::from(output_tokens) * pricing.output_per_million / per_million
                * multiplier,
        };
        Some((pricing, cost))
    }

    fn insert(
        &mut self,
        model: &str,
        input: Decimal,
        cached_input: Decimal,
        output: Decimal,
        aliases: &[String],
    ) {
        let canonical = normalize_model_key(model);
        let pricing = ModelPricing {
            model: canonical.clone(),
            input_per_million: input,
            cached_input_per_million: cached_input,
            output_per_million: output,
        };
        self.models.insert(canonical, pricing.clone());
        for alias in aliases {
            self.models
                .insert(normalize_model_key(alias), pricing.clone());
        }
    }
}

pub fn default_pricing_override_path(cache_dir: &Path) -> PathBuf {
    cache_dir
        .parent()
        .unwrap_or(cache_dir)
        .join("model-pricing.json")
}

pub fn model_pricing_candidates(raw: &str) -> Vec<String> {
    let normalized = normalize_model_key(raw);
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut variants = vec![normalized.clone()];
    if let Some((head, _)) = normalized.rsplit_once('@') {
        variants.push(head.to_string());
    }

    for value in variants {
        push_candidate(&mut candidates, &mut seen, value.clone());
        push_candidate(&mut candidates, &mut seen, strip_model_date_suffix(&value));
        for suffix in ["-xhigh", "-high", "-medium", "-low", "-minimal"] {
            if let Some(head) = value.strip_suffix(suffix) {
                push_candidate(&mut candidates, &mut seen, head.to_string());
                push_candidate(&mut candidates, &mut seen, strip_model_date_suffix(head));
            }
        }
    }

    candidates
}

fn push_candidate(candidates: &mut Vec<String>, seen: &mut BTreeSet<String>, candidate: String) {
    if seen.insert(candidate.clone()) {
        candidates.push(candidate);
    }
}

fn normalize_model_key(raw: &str) -> String {
    let mut model = raw.trim().to_ascii_lowercase();
    if let Some((_, suffix)) = model.rsplit_once('/') {
        model = suffix.to_string();
    }
    model
}

fn strip_model_date_suffix(model: &str) -> String {
    if model.len() > 11 && model.is_char_boundary(model.len() - 11) {
        let suffix = &model[model.len() - 11..];
        if suffix.as_bytes().first() == Some(&b'-')
            && suffix[1..5].chars().all(|value| value.is_ascii_digit())
            && suffix.as_bytes().get(5) == Some(&b'-')
            && suffix[6..8].chars().all(|value| value.is_ascii_digit())
            && suffix.as_bytes().get(8) == Some(&b'-')
            && suffix[9..11].chars().all(|value| value.is_ascii_digit())
        {
            return model[..model.len() - 11].to_string();
        }
    }
    if let Some((head, suffix)) = model.rsplit_once('-') {
        if suffix.len() == 8 && suffix.chars().all(|value| value.is_ascii_digit()) {
            return head.to_string();
        }
    }
    model.to_string()
}

fn parse_nonnegative_decimal(path: &Path, model: &str, field: &str, raw: &str) -> Result<Decimal> {
    let value = Decimal::from_str(raw.trim()).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "invalid {field} for model {model} in {}: {source}",
            path.display()
        ))
    })?;
    if value < Decimal::ZERO {
        return Err(CompanionError::InvalidConfig(format!(
            "{field} for model {model} in {} must not be negative",
            path.display()
        )));
    }
    Ok(value)
}

fn decimal(raw: &str) -> Decimal {
    Decimal::from_str(raw).expect("built-in pricing decimal")
}

fn format_usd(value: Decimal) -> String {
    value.round_dp(8).normalize().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_provider_prefix_dates_and_reasoning_suffixes() {
        let catalog = PricingCatalog::builtin();
        assert_eq!(
            catalog
                .find("openai/gpt-5.4")
                .map(|pricing| pricing.model.as_str()),
            Some("gpt-5.4")
        );
        assert_eq!(
            catalog
                .find("GPT-5.4-2026-03-05")
                .map(|pricing| pricing.model.as_str()),
            Some("gpt-5.4")
        );
        assert_eq!(
            catalog
                .find("gpt-5.3-codex@high")
                .map(|pricing| pricing.model.as_str()),
            Some("gpt-5.3-codex")
        );
        assert_eq!(
            catalog
                .find("openai/gpt-5.4-2026-03-05@high")
                .map(|pricing| pricing.model.as_str()),
            Some("gpt-5.4")
        );
        assert!(catalog.find("gpt-5.2-codex@high").is_none());
        assert!(catalog.find("unknown-model").is_none());
    }

    #[test]
    fn splits_fresh_cached_and_output_cost() {
        let catalog = PricingCatalog::builtin();
        let (_, cost) = catalog
            .estimate("gpt-5.4", None, 1_000_000, 1_000_000, 1_000_000)
            .expect("pricing");
        assert_eq!(cost.fresh_input_usd, decimal("2.50"));
        assert_eq!(cost.cached_input_usd, decimal("0.25"));
        assert_eq!(cost.output_usd, decimal("15.00"));
        assert_eq!(cost.total_usd(), decimal("17.75"));
    }

    #[test]
    fn override_adds_alias_and_provider_multiplier() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("model-pricing.json");
        fs::write(
            &path,
            r#"{
              "models": [{
                "model": "custom-model",
                "aliases": ["vendor/custom-latest"],
                "inputPerMillion": "1",
                "cachedInputPerMillion": "0.1",
                "outputPerMillion": "2"
              }],
              "providerMultipliers": {
                "paid-relay": "1.5"
              }
            }"#,
        )
        .expect("pricing file");

        let catalog = PricingCatalog::builtin()
            .load_override(&path)
            .expect("override");
        let (pricing, cost) = catalog
            .estimate(
                "vendor/custom-latest",
                Some("paid-relay"),
                1_000_000,
                0,
                1_000_000,
            )
            .expect("custom pricing");
        assert_eq!(pricing.model, "custom-model");
        assert_eq!(cost.total_usd(), decimal("4.5"));
        assert_eq!(catalog.override_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn exact_snapshot_override_wins_before_base_model_fallback() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("model-pricing.json");
        fs::write(
            &path,
            r#"{
              "models": [{
                "model": "gpt-5.4-2026-03-05",
                "inputPerMillion": "9",
                "cachedInputPerMillion": "0.9",
                "outputPerMillion": "18"
              }]
            }"#,
        )
        .expect("pricing file");

        let catalog = PricingCatalog::builtin()
            .load_override(&path)
            .expect("override");
        let (pricing, cost) = catalog
            .estimate("gpt-5.4-2026-03-05", None, 1_000_000, 0, 0)
            .expect("snapshot pricing");
        assert_eq!(pricing.model, "gpt-5.4-2026-03-05");
        assert_eq!(cost.fresh_input_usd, decimal("9"));
    }
}
