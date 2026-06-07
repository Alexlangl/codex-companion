use crate::types::ProviderUpsert;
use crate::validate::{validate_base_url, validate_id};
use codex_companion_core::{
    ConfigStore, ProviderConfig, ProviderGroup, ProviderHealth, Result, DEFAULT_GROUP_ID,
};

pub fn add_provider(store: &ConfigStore, input: ProviderUpsert) -> Result<ProviderConfig> {
    validate_id(&input.id)?;
    validate_base_url(&input.base_url)?;
    store.update(|config| {
        let provider = ProviderConfig {
            id: input.id.trim().to_string(),
            name: input.name.trim().to_string(),
            kind: input.kind,
            base_url: input.base_url.trim().trim_end_matches('/').to_string(),
            auth_ref: input.auth_ref.filter(|value| !value.trim().is_empty()),
            direct_auth_ref: input
                .direct_auth_ref
                .filter(|value| !value.trim().is_empty()),
            model_map: input.model_map,
            priority: input.priority,
            enabled: input.enabled,
            refresh_interval_seconds: input.refresh_interval_seconds,
            account: input.account,
        };
        config
            .health
            .entry(provider.id.clone())
            .or_insert_with(ProviderHealth::default);
        config
            .providers
            .insert(provider.id.clone(), provider.clone());
        let group = config
            .groups
            .entry(DEFAULT_GROUP_ID.to_string())
            .or_insert_with(ProviderGroup::default_group);
        if !group.provider_order.contains(&provider.id) {
            group.provider_order.push(provider.id.clone());
        }
        Ok(provider)
    })
}

pub fn remove_provider(store: &ConfigStore, id: &str) -> Result<bool> {
    store.update(|config| {
        let removed = config.providers.remove(id).is_some();
        config.health.remove(id);
        for group in config.groups.values_mut() {
            group.provider_order.retain(|provider_id| provider_id != id);
        }
        Ok(removed)
    })
}

pub fn list_providers(store: &ConfigStore) -> Result<Vec<ProviderConfig>> {
    let config = store.load()?;
    Ok(config.providers.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{default_refresh_interval_seconds, GroupPolicy, ProviderKind};
    use std::collections::BTreeMap;

    fn provider(id: &str) -> ProviderUpsert {
        ProviderUpsert {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn remove_provider_prunes_group_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        add_provider(&store, provider("a")).expect("add a");
        add_provider(&store, provider("b")).expect("add b");
        store
            .update(|config| {
                config.groups.insert(
                    "work".to_string(),
                    ProviderGroup {
                        id: "work".to_string(),
                        name: "Work".to_string(),
                        policy: GroupPolicy::PriorityFallback,
                        provider_order: vec!["a".to_string(), "b".to_string()],
                        fallback_enabled: true,
                    },
                );
                Ok(())
            })
            .expect("group");

        assert!(remove_provider(&store, "a").expect("remove"));
        let config = store.load().expect("load");
        assert_eq!(config.groups["work"].provider_order, vec!["b".to_string()]);
        assert_eq!(
            config.groups[DEFAULT_GROUP_ID].provider_order,
            vec!["b".to_string()]
        );
    }
}
