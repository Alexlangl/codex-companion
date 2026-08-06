use codex_companion_core::{CompanionError, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const MANAGED_MODEL_CATALOG_FILENAME: &str = "codex-companion-model-catalog.json";
const MODELS_CACHE_FILENAME: &str = "models_cache.json";
const MODEL_CATALOG_MAX_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_MODEL_SLUG: &str = "gpt-5.6-sol";

pub(crate) fn managed_model_catalog_path(codex_dir: &Path) -> PathBuf {
    codex_dir.join(MANAGED_MODEL_CATALOG_FILENAME)
}

pub(crate) fn normalized_model_slugs(
    requested: &[String],
    configured_model: Option<&str>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for model in configured_model
        .into_iter()
        .chain(requested.iter().map(String::as_str))
    {
        let model = model.trim();
        if model.is_empty() || model == "default" || !seen.insert(model.to_string()) {
            continue;
        }
        models.push(model.to_string());
    }
    if models.is_empty() {
        models.push(DEFAULT_MODEL_SLUG.to_string());
    }
    models
}

pub(crate) fn build_model_catalog(codex_dir: &Path, model_slugs: &[String]) -> Result<Vec<u8>> {
    let fallback = fallback_template()?;
    let cached_models = load_cached_models(codex_dir);
    let cached_template = cached_models
        .iter()
        .find(|model| model_slug(model) == Some(DEFAULT_MODEL_SLUG))
        .cloned();

    let models = model_slugs
        .iter()
        .enumerate()
        .map(|(priority, slug)| {
            let exact = cached_models
                .iter()
                .find(|model| model_slug(model).is_some_and(|value| value == slug))
                .cloned();
            let exact_match = exact.is_some();
            let mut entry = exact
                .or_else(|| cached_template.clone())
                .unwrap_or_else(|| fallback.clone());
            merge_missing_fields(&mut entry, &fallback);
            customize_entry(&mut entry, slug, priority, exact_match);
            entry
        })
        .collect::<Vec<_>>();

    let mut text = serde_json::to_vec_pretty(&json!({ "models": models })).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex 模型目录失败: {source}"))
    })?;
    text.push(b'\n');
    Ok(text)
}

fn load_cached_models(codex_dir: &Path) -> Vec<Value> {
    let path = codex_dir.join(MODELS_CACHE_FILENAME);
    let Ok(metadata) = fs::metadata(&path) else {
        return Vec::new();
    };
    if metadata.len() > MODEL_CATALOG_MAX_BYTES {
        return Vec::new();
    }
    fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|catalog| catalog.get("models").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn fallback_template() -> Result<Value> {
    serde_json::from_str(include_str!("model_catalog_template.json")).map_err(|source| {
        CompanionError::InvalidConfig(format!("内置 Codex 模型目录模板无效: {source}"))
    })
}

fn model_slug(model: &Value) -> Option<&str> {
    model.get("slug").and_then(Value::as_str)
}

fn merge_missing_fields(entry: &mut Value, fallback: &Value) {
    let (Some(entry), Some(fallback)) = (entry.as_object_mut(), fallback.as_object()) else {
        return;
    };
    for (key, value) in fallback {
        entry.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

fn customize_entry(entry: &mut Value, slug: &str, priority: usize, exact_match: bool) {
    let Some(entry) = entry.as_object_mut() else {
        return;
    };
    entry.insert("slug".to_string(), Value::String(slug.to_string()));
    if !exact_match {
        entry.insert("display_name".to_string(), Value::String(slug.to_string()));
        entry.insert("description".to_string(), Value::String(slug.to_string()));
    }
    entry.insert("priority".to_string(), json!(1000 + priority));
    entry.insert("additional_speed_tiers".to_string(), json!([]));
    entry.insert("service_tiers".to_string(), json!([]));
    entry.insert("availability_nux".to_string(), Value::Null);
    entry.insert("upgrade".to_string(), Value::Null);

    if known_ultra_model(slug) {
        ensure_ultra_reasoning(entry);
        entry.insert(
            "multi_agent_version".to_string(),
            Value::String("v2".to_string()),
        );
    } else {
        remove_ultra_reasoning(entry);
        entry.remove("multi_agent_version");
    }
}

fn known_ultra_model(slug: &str) -> bool {
    matches!(slug, "gpt-5.6-sol" | "gpt-5.6-terra")
}

fn ensure_ultra_reasoning(entry: &mut Map<String, Value>) {
    let levels = entry
        .entry("supported_reasoning_levels".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(levels) = levels.as_array_mut() else {
        return;
    };
    if levels
        .iter()
        .any(|level| level.get("effort").and_then(Value::as_str) == Some("ultra"))
    {
        return;
    }
    levels.push(json!({
        "effort": "ultra",
        "description": "Maximum reasoning with automatic task delegation"
    }));
}

fn remove_ultra_reasoning(entry: &mut Map<String, Value>) {
    if let Some(levels) = entry
        .get_mut("supported_reasoning_levels")
        .and_then(Value::as_array_mut)
    {
        levels.retain(|level| level.get("effort").and_then(Value::as_str) != Some("ultra"));
    }
    if entry.get("default_reasoning_level").and_then(Value::as_str) == Some("ultra") {
        entry.insert(
            "default_reasoning_level".to_string(),
            Value::String("high".to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_model_is_first_and_duplicates_are_removed() {
        let models = normalized_model_slugs(
            &[
                "gpt-5.6-terra".to_string(),
                "gpt-5.6-sol".to_string(),
                "default".to_string(),
            ],
            Some("gpt-5.6-sol"),
        );

        assert_eq!(models, vec!["gpt-5.6-sol", "gpt-5.6-terra"]);
    }

    #[test]
    fn falls_back_to_ultra_capable_sol_model() {
        assert_eq!(normalized_model_slugs(&[], None), vec![DEFAULT_MODEL_SLUG]);
    }

    #[test]
    fn generated_catalog_keeps_ultra_and_does_not_advertise_fast_tier() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bytes =
            build_model_catalog(temp.path(), &["gpt-5.6-sol".to_string()]).expect("catalog");
        let catalog: Value = serde_json::from_slice(&bytes).expect("json");
        let model = &catalog["models"][0];

        assert_eq!(model["slug"], "gpt-5.6-sol");
        assert!(model["supported_reasoning_levels"]
            .as_array()
            .expect("levels")
            .iter()
            .any(|level| level["effort"] == "ultra"));
        assert_eq!(model["multi_agent_version"], "v2");
        assert_eq!(model["service_tiers"], json!([]));
        assert_eq!(model["additional_speed_tiers"], json!([]));
    }

    #[test]
    fn known_ultra_model_overrides_stale_cached_multi_agent_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join(MODELS_CACHE_FILENAME),
            json!({
                "models": [{
                    "slug": "gpt-5.6-sol",
                    "display_name": "Sol",
                    "description": "Sol",
                    "base_instructions": "test",
                    "supported_reasoning_levels": [{"effort": "max"}],
                    "multi_agent_version": "v1"
                }]
            })
            .to_string(),
        )
        .expect("cache");

        let bytes =
            build_model_catalog(temp.path(), &["gpt-5.6-sol".to_string()]).expect("catalog");
        let catalog: Value = serde_json::from_slice(&bytes).expect("json");
        let model = &catalog["models"][0];

        assert_eq!(model["multi_agent_version"], "v2");
        assert!(model["supported_reasoning_levels"]
            .as_array()
            .expect("levels")
            .iter()
            .any(|level| level["effort"] == "ultra"));
    }

    #[test]
    fn unknown_model_does_not_inherit_ultra_or_multi_agent_v2() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bytes =
            build_model_catalog(temp.path(), &["custom-model".to_string()]).expect("catalog");
        let catalog: Value = serde_json::from_slice(&bytes).expect("json");
        let model = &catalog["models"][0];

        assert!(!model["supported_reasoning_levels"]
            .as_array()
            .expect("levels")
            .iter()
            .any(|level| level["effort"] == "ultra"));
        assert!(model.get("multi_agent_version").is_none());
    }

    #[test]
    fn exact_cached_capabilities_win_for_models_without_ultra() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join(MODELS_CACHE_FILENAME),
            json!({
                "models": [{
                    "slug": "limited-model",
                    "display_name": "Limited",
                    "description": "Limited",
                    "base_instructions": "test",
                    "supported_reasoning_levels": [{"effort": "high"}]
                }]
            })
            .to_string(),
        )
        .expect("cache");

        let bytes =
            build_model_catalog(temp.path(), &["limited-model".to_string()]).expect("catalog");
        let catalog: Value = serde_json::from_slice(&bytes).expect("json");
        let levels = catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .expect("levels");

        assert_eq!(catalog["models"][0]["display_name"], "Limited");
        assert!(!levels.iter().any(|level| level["effort"] == "ultra"));
        assert!(catalog["models"][0].get("multi_agent_version").is_none());
    }
}
