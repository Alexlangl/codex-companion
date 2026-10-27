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
            "gpt-6.1-sol" => 0,
            "gpt-6.1-terra" => 1,
            "gpt-6-astra" => 2,
            "gpt-6-sol" => 3,
            "gpt-6-luna" => 4,
            "gpt-5.6-sol" => 5,
            "gpt-5.6-terra" => 6,
            "gpt-5.6-luna" => 7,
            _ => 8,
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
        models.retain(|model| {
            model
                .get("slug")
                .and_then(Value::as_str)
                .is_some_and(|slug| slug.starts_with("gpt-"))
        });
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
            if !slug.starts_with("gpt-") {
                return None;
            }
            let mut model = template.clone();
            model["slug"] = json!(slug);
            model["display_name"] = json!(slug);
            model["description"] = json!(slug);
            if matches!(slug, "gpt-6.1-sol" | "gpt-6.1-terra") {
                if let Some(fields) = model.as_object_mut() {
                    for field in [
                        "context_window",
                        "max_context_window",
                        "auto_compact_token_limit",
                        "default_reasoning_level",
                        "supported_reasoning_levels",
                    ] {
                        fields.remove(field);
                    }
                }
            }
            // A model id alone does not establish Codex-specific capabilities.
            if !codex_companion_core::known_codex_ultra_model(slug) {
                model.as_object_mut().unwrap().remove("multi_agent_version");
                if let Some(levels) = model
                    .get_mut("supported_reasoning_levels")
                    .and_then(Value::as_array_mut)
                {
                    levels.retain(|level| {
                        level.get("effort").and_then(Value::as_str) != Some("ultra")
                    });
                }
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
    fn id_only_new_sol_gets_its_own_capabilities() {
        let mut catalog = json!({"data":[{"id":"gpt-6.1-sol"}]});
        adapt_codex_model_catalog(&mut catalog);
        let model = &catalog["models"][0];
        assert_eq!(model["context_window"], 1_050_000);
        assert_eq!(model["default_reasoning_level"], "medium");
        assert!(model.get("multi_agent_version").is_none());
        assert!(model["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|level| level["effort"] != "ultra"));
    }

    #[test]
    fn id_only_new_terra_gets_its_own_capabilities() {
        let mut catalog = json!({"data":[{"id":"gpt-6.1-terra"}]});
        adapt_codex_model_catalog(&mut catalog);
        let model = &catalog["models"][0];
        assert_eq!(model["display_name"], "GPT-6.1 Terra");
        assert_eq!(model["context_window"], 1_050_000);
        assert_eq!(model["auto_compact_token_limit"], 829_800);
        assert_eq!(model["default_reasoning_level"], "medium");
        assert!(model.get("multi_agent_version").is_none());
        assert!(model["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|level| level["effort"] == "none"));
        assert!(model["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|level| level["effort"] != "ultra"));
    }

    #[test]
    fn merged_catalog_orders_terra_after_sol_without_adding_unavailable_models() {
        let mut catalog =
            json!({"data":[{"id":"gpt-6.1-terra"},{"id":"gpt-5.6-luna"},{"id":"aaa-custom"}]});
        let sol = json!({"data":[{"id":"gpt-6.1-sol"},{"id":"gpt-6.1-terra"}]});
        merge_codex_catalogs(&mut catalog, vec![sol]);
        let slugs = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(slugs, vec!["gpt-6.1-sol", "gpt-6.1-terra", "gpt-5.6-luna"]);
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
        let mut value =
            json!({"data":[{"id":"gpt-custom"},{"id":"deepseek-flash"},{"id":""},{"other":true}]});
        let original = value["data"].clone();
        adapt_codex_model_catalog(&mut value);
        assert_eq!(value["data"], original);
        assert_eq!(value["models"].as_array().unwrap().len(), 1);
        assert_eq!(value["models"][0]["slug"], "gpt-custom");
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
    fn native_catalog_keeps_shape_while_receiving_compaction_metadata() {
        for mut value in [
            json!({"models":[],"data":[{"id":"excluded"}]}),
            json!({"models":[{"slug":"gpt-native","custom":true},{"slug":"kimi-k3"}]}),
        ] {
            adapt_codex_model_catalog(&mut value);
            if let Some(model) = value["models"].as_array().and_then(|models| models.first()) {
                assert_eq!(value["models"].as_array().unwrap().len(), 1);
                assert_eq!(model["slug"], "gpt-native");
                assert_eq!(
                    model["comp_hash"],
                    codex_companion_core::CODEX_CLIENT_COMP_HASH
                );
            }
        }
    }
}
