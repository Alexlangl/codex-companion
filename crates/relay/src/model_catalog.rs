use serde_json::{json, Value};

/// Preserve native catalog metadata. Only synthesize entries for OpenAI-style lists.
pub(crate) fn adapt_codex_model_catalog(value: &mut Value) {
    if value.get("models").is_some_and(Value::is_array) {
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
            if !matches!(slug, "gpt-5.6-sol" | "gpt-5.6-terra") {
                model.as_object_mut().unwrap().remove("multi_agent_version");
                model["supported_reasoning_levels"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|level| level.get("effort").and_then(Value::as_str) != Some("ultra"));
            }
            Some(model)
        })
        .collect::<Vec<_>>();
    value["models"] = json!(models);
}

#[cfg(test)]
mod tests {
    use super::*;
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
