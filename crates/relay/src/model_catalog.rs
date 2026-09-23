use serde_json::{json, Value};

pub(crate) fn merge_codex_catalogs(primary: &mut Value, catalogs: Vec<Value>) {
    adapt_codex_model_catalog(primary);
    let Some(models) = primary.get_mut("models").and_then(Value::as_array_mut) else {
        return;
    };
    for mut catalog in catalogs {
        adapt_codex_model_catalog(&mut catalog);
        for model in catalog
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(slug) = model.get("slug").and_then(Value::as_str) else {
                continue;
            };
            if !models
                .iter()
                .any(|existing| existing.get("slug").and_then(Value::as_str) == Some(slug))
            {
                models.push(model.clone());
            }
        }
    }
    // Provider affinity or round-robin must not change the model picker's order.
    models.sort_by(|a, b| {
        let rank = |value: &Value| match value.get("slug").and_then(Value::as_str).unwrap_or("") {
            "gpt-6-astra" => 0,
            "gpt-6-sol" => 1,
            "gpt-6-luna" => 2,
            "gpt-5.6-sol" => 3,
            "gpt-5.6-terra" => 4,
            "gpt-5.6-luna" => 5,
            _ => 6,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a["slug"].as_str().cmp(&b["slug"].as_str()))
    });
    for (priority, model) in models.iter_mut().enumerate() {
        model["priority"] = json!(priority);
    }
}

/// Keep native capabilities, repairing only known generic reasoning placeholders.
pub(crate) fn adapt_codex_model_catalog(value: &mut Value) {
    if let Some(models) = value.get_mut("models").and_then(Value::as_array_mut) {
        for model in models {
            codex_companion_core::repair_codex_model_metadata(model);
        }
        return;
    }
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return;
    };
    let template: Value = serde_json::from_str(codex_companion_core::CODEX_MODEL_CATALOG_TEMPLATE)
        .expect("embedded Codex catalog template");
    let models = data
        .iter()
        .filter_map(|item| {
            let slug = item.get("id").and_then(Value::as_str)?.trim();
            if slug.is_empty() {
                return None;
            }
            let mut model = template.clone();
            model["slug"] = json!(slug);
            model["display_name"] = json!(slug);
            model["description"] = json!(slug);
            // A model id alone does not establish Codex-specific capabilities.
            if !codex_companion_core::known_codex_ultra_model(slug) {
                model.as_object_mut().unwrap().remove("multi_agent_version");
                model["supported_reasoning_levels"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|level| level.get("effort").and_then(Value::as_str) != Some("ultra"));
            }
            codex_companion_core::repair_codex_model_metadata(&mut model);
            Some(model)
        })
        .collect::<Vec<_>>();
    value["models"] = json!(models);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_generic_gpt6_entries_gain_reasoning_without_changing_membership() {
        let mut catalog = json!({"models":[{"slug":"gpt-6-sol", "supported_reasoning_levels":[{"effort":"none"}],"default_reasoning_level":"none"}]});
        adapt_codex_model_catalog(&mut catalog);
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert!(catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["effort"] == "ultra"));
    }

    #[test]
    fn group_catalog_keeps_models_across_provider_order_changes() {
        let sol = json!({"data":[{"id":"gpt-6-sol"},{"id":"gpt-5.6-sol"}]});
        let luna = json!({"data":[{"id":"gpt-6-luna"},{"id":"gpt-5.6-sol"}]});
        let mut first = sol.clone();
        merge_codex_catalogs(&mut first, vec![luna.clone()]);
        let mut second = luna;
        merge_codex_catalogs(&mut second, vec![sol]);
        assert_eq!(first["models"], second["models"]);
        assert_eq!(first["models"].as_array().unwrap().len(), 3);
        assert_eq!(first["models"][0]["slug"], "gpt-6-sol");
        assert_eq!(first["models"][1]["slug"], "gpt-6-luna");
    }
    #[test]
    fn adds_native_schema_without_losing_standard_api_list() {
        let mut value = json!({"data":[{"id":"my-model"},{"id":""},{"other":true}]});
        let original = value["data"].clone();
        adapt_codex_model_catalog(&mut value);
        assert_eq!(value["data"], original);
        assert_eq!(value["models"].as_array().unwrap().len(), 1);
        assert_eq!(value["models"][0]["slug"], "my-model");
        for required in [
            "shell_type",
            "visibility",
            "supported_in_api",
            "base_instructions",
            "truncation_policy",
        ] {
            assert!(value["models"][0].get(required).is_some());
        }
    }
    #[test]
    fn native_catalog_is_unchanged_including_empty_catalog() {
        for mut value in [
            json!({"models":[],"data":[{"id":"excluded"}]}),
            json!({"models":[{"slug":"native","custom":true}]}),
        ] {
            let before = value.clone();
            adapt_codex_model_catalog(&mut value);
            assert_eq!(value, before);
        }
    }
}
