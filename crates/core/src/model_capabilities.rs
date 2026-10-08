use serde_json::{json, Value};

/// Keep the compaction format consistent across managed and upstream catalogs.
pub const CODEX_CLIENT_COMP_HASH: &str = "3000";
pub const AUTO_COMPACT_RATIO_PERCENT: i64 = 90;

/// Return known model context limits when a provider omits them. The first
/// value is the default window and the second is the opt-in maximum window.
pub fn known_codex_context_limits(slug: &str) -> Option<(i64, i64)> {
    match slug {
        "gpt-6.1-sol" => Some((1_050_000, 1_050_000)),
        "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna" => Some((256_000, 256_000)),
        "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => Some((272_000, 921_000)),
        "gpt-5.5" => Some((272_000, 272_000)),
        _ => None,
    }
}

pub fn derived_auto_compact_token_limit(context_window: i64) -> i64 {
    if context_window <= 0 {
        return 0;
    }
    context_window.saturating_mul(AUTO_COMPACT_RATIO_PERCENT) / 100
}

/// Preserve explicit valid thresholds, deriving only missing or invalid ones.
pub fn apply_codex_compaction_metadata(model: &mut Value) {
    let Some(object) = model.as_object_mut() else {
        return;
    };
    object.insert("comp_hash".to_string(), json!(CODEX_CLIENT_COMP_HASH));
    let Some(context_window) = object
        .get("context_window")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
    else {
        return;
    };
    let valid_compact_limit = object
        .get("auto_compact_token_limit")
        .and_then(Value::as_i64)
        .is_some_and(|value| value > 0 && value < context_window);
    if !valid_compact_limit {
        object.insert(
            "auto_compact_token_limit".to_string(),
            json!(derived_auto_compact_token_limit(context_window)),
        );
    }
}

/// Fill the context metadata used by Codex clients without replacing an
/// explicit, valid value from a provider catalog.
pub fn repair_codex_context_metadata(model: &mut Value) {
    let Some(slug) = model.get("slug").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    let Some((known_context_window, known_max_context_window)) = known_codex_context_limits(&slug)
    else {
        return;
    };
    if model
        .get("context_window")
        .and_then(Value::as_i64)
        .is_none_or(|value| value <= 0)
    {
        model["context_window"] = json!(known_context_window);
    }
    let current_context_window = model.get("context_window").and_then(Value::as_i64);
    let current_max_context_window = model.get("max_context_window").and_then(Value::as_i64);
    if current_max_context_window.is_none_or(|value| value <= 0)
        || (known_max_context_window > known_context_window
            && current_context_window == Some(known_context_window)
            && current_max_context_window == Some(known_context_window))
    {
        model["max_context_window"] = json!(known_max_context_window);
    }
    apply_codex_compaction_metadata(model);
}

pub fn known_codex_ultra_model(slug: &str) -> bool {
    matches!(
        slug,
        "gpt-6-astra" | "gpt-6-sol" | "gpt-5.6-sol" | "gpt-5.6-terra"
    )
}

/// Repair generic upstream placeholders for known models without overwriting
/// deliberate reasoning subsets or custom display names in native catalogs.
pub fn repair_codex_model_metadata(model: &mut Value) {
    let Some(slug) = model.get("slug").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    repair_codex_context_metadata(model);
    apply_codex_compaction_metadata(model);
    let name = match slug.as_str() {
        "gpt-6.1-sol" => "GPT-6.1 Sol",
        "gpt-6-astra" => "GPT-6 Astra",
        "gpt-6-sol" => "GPT-6 Sol",
        "gpt-6-luna" => "GPT-6 Luna",
        "gpt-5.6-sol" => "GPT-5.6 Sol",
        "gpt-5.6-terra" => "GPT-5.6 Terra",
        "gpt-5.6-luna" => "GPT-5.6 Luna",
        "gpt-5.5" => "GPT-5.5",
        _ => return,
    };
    let placeholder = model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .is_none_or(|levels| {
            levels.is_empty()
                || levels.iter().all(|level| {
                    matches!(
                        level
                            .get("effort")
                            .and_then(Value::as_str)
                            .or_else(|| level.as_str()),
                        None | Some("none")
                    )
                })
        });
    if placeholder {
        let levels = ["low", "medium", "high", "xhigh", "max", "ultra"]
            .into_iter()
            .filter(|effort| (*effort != "ultra" || known_codex_ultra_model(&slug))
                && (*effort != "max" || slug != "gpt-5.5"))
            .map(|effort| json!({"effort": effort, "description": format!("Reasoning effort: {effort}")}))
            .collect::<Vec<_>>();
        model["supported_reasoning_levels"] = json!(levels);
        model["default_reasoning_level"] = json!(if slug == "gpt-5.6-sol" {
            "low"
        } else {
            "medium"
        });
    }
    let old_short_name = name.strip_prefix("GPT-").unwrap_or(name);
    let old_hyphenated_name = name.replace(' ', "-");
    if model
        .get("display_name")
        .and_then(Value::as_str)
        .is_none_or(|current| {
            current == slug || current == old_short_name || current == old_hyphenated_name
        })
    {
        model["display_name"] = json!(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_models_repair_none_placeholders_but_preserve_custom_subsets() {
        for slug in ["gpt-6-sol", "gpt-6-luna"] {
            let mut model = json!({"slug":slug,"display_name":slug,
                "default_reasoning_level":"none","supported_reasoning_levels":[{"effort":"none"}]});
            repair_codex_model_metadata(&mut model);
            let levels = model["supported_reasoning_levels"].as_array().unwrap();
            assert!(levels.iter().any(|level| level["effort"] == "max"));
            assert_eq!(
                levels.iter().any(|level| level["effort"] == "ultra"),
                slug == "gpt-6-sol"
            );
            assert_eq!(model["default_reasoning_level"], "medium");
            let mut custom = json!({"slug":slug,"display_name":"My model", "supported_reasoning_levels":[{"effort":"high"}]});
            repair_codex_model_metadata(&mut custom);
            assert_eq!(custom["display_name"], "My model");
            assert_eq!(
                custom["supported_reasoning_levels"],
                json!([{"effort":"high"}])
            );
            assert_eq!(custom["context_window"], 256_000);
        }
    }

    #[test]
    fn context_limits_and_compaction_follow_the_current_catalog() {
        let mut model = json!({"slug":"gpt-6-sol"});
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["context_window"], 256_000);
        assert_eq!(model["max_context_window"], 256_000);
        assert_eq!(model["auto_compact_token_limit"], 230_400);

        let mut astra = json!({
            "slug":"gpt-6-astra",
            "context_window":256_000,
            "auto_compact_token_limit":256_000
        });
        repair_codex_context_metadata(&mut astra);
        assert_eq!(astra["max_context_window"], 256_000);
        assert_eq!(astra["auto_compact_token_limit"], 230_400);
    }

    #[test]
    fn current_codex_defaults_preserve_sol_as_the_fast_default() {
        let mut model = json!({"slug":"gpt-5.6-sol"});
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["default_reasoning_level"], "low");
        assert_eq!(model["context_window"], 272_000);
        assert_eq!(model["max_context_window"], 921_000);
        assert_eq!(model["auto_compact_token_limit"], 244_800);
    }

    #[test]
    fn gpt_6_1_sol_uses_official_fallback_without_ultra() {
        let mut model = json!({"slug":"gpt-6.1-sol", "display_name":"gpt-6.1-sol",
            "default_reasoning_level":"none", "supported_reasoning_levels":[{"effort":"none"}]});
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["display_name"], "GPT-6.1 Sol");
        assert_eq!(model["default_reasoning_level"], "medium");
        assert_eq!(model["context_window"], 1_050_000);
        assert_eq!(model["max_context_window"], 1_050_000);
        assert_eq!(model["auto_compact_token_limit"], 945_000);
        assert_eq!(model["comp_hash"], CODEX_CLIENT_COMP_HASH);
        assert_eq!(
            model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|level| level["effort"].as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn valid_provider_context_and_compaction_values_are_preserved() {
        let mut model = json!({
            "slug":"gpt-6-sol",
            "context_window":516_000,
            "max_context_window":1_000_000,
            "auto_compact_token_limit":460_000
        });
        repair_codex_context_metadata(&mut model);
        assert_eq!(model["context_window"], 516_000);
        assert_eq!(model["max_context_window"], 1_000_000);
        assert_eq!(model["auto_compact_token_limit"], 460_000);
    }

    #[test]
    fn compaction_metadata_is_unified_and_threshold_is_derived() {
        let mut model = json!({"slug":"custom", "context_window":516_000});
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["comp_hash"], CODEX_CLIENT_COMP_HASH);
        assert_eq!(model["auto_compact_token_limit"], 464_400);
        model["auto_compact_token_limit"] = json!(460_000);
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["auto_compact_token_limit"], 460_000);
        model["auto_compact_token_limit"] = json!(516_000);
        repair_codex_model_metadata(&mut model);
        assert_eq!(model["auto_compact_token_limit"], 464_400);
    }
}
