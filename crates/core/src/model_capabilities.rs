use serde_json::{json, Value};

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
    let name = match slug.as_str() {
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
        model["default_reasoning_level"] = json!("medium");
    }
    let old_short_name = name.strip_prefix("GPT-").unwrap_or(name);
    if model
        .get("display_name")
        .and_then(Value::as_str)
        .is_none_or(|current| current == slug || current == old_short_name)
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
            let before = custom.clone();
            repair_codex_model_metadata(&mut custom);
            assert_eq!(custom, before);
        }
    }
}
