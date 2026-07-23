use crate::types::GroupUpsert;
use crate::validate::validate_id;
use codex_companion_core::{
    CompanionConfig, CompanionError, ConfigStore, GroupPolicy, HealthStatusKind, ProviderConfig,
    ProviderGroup, ProviderHealth, Result,
};
use std::collections::BTreeMap;

pub fn upsert_group(store: &ConfigStore, input: GroupUpsert) -> Result<ProviderGroup> {
    validate_id(&input.id)?;
    store.update(|config| {
        for provider_id in &input.provider_order {
            if !config.providers.contains_key(provider_id) {
                return Err(CompanionError::InvalidConfig(format!(
                    "unknown provider in group: {provider_id}"
                )));
            }
        }
        let group = ProviderGroup {
            id: input.id.trim().to_string(),
            name: input.name.trim().to_string(),
            policy: input.policy,
            provider_order: input.provider_order,
            provider_weights: input.provider_weights,
            fallback_enabled: input.fallback_enabled,
        };
        config.groups.insert(group.id.clone(), group.clone());
        Ok(group)
    })
}

pub fn use_group(store: &ConfigStore, id: &str) -> Result<ProviderGroup> {
    store.update(|config| {
        let group = config
            .groups
            .get(id)
            .cloned()
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {id}")))?;
        config.relay.active_group_id = id.to_string();
        Ok(group)
    })
}

pub fn set_group_order(
    store: &ConfigStore,
    id: &str,
    provider_order: Vec<String>,
) -> Result<ProviderGroup> {
    store.update(|config| {
        for provider_id in &provider_order {
            if !config.providers.contains_key(provider_id) {
                return Err(CompanionError::InvalidConfig(format!(
                    "unknown provider in group: {provider_id}"
                )));
            }
        }
        let group = config
            .groups
            .get_mut(id)
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {id}")))?;
        group.provider_order = provider_order;
        Ok(group.clone())
    })
}

pub fn active_group(config: &CompanionConfig) -> Option<ProviderGroup> {
    config.groups.get(&config.relay.active_group_id).cloned()
}

pub fn selected_providers(config: &CompanionConfig) -> Vec<ProviderConfig> {
    let Some(group) = config.groups.get(&config.relay.active_group_id) else {
        return Vec::new();
    };
    selected_providers_for_group(config, group)
}

pub fn selected_providers_for_group(
    config: &CompanionConfig,
    group: &ProviderGroup,
) -> Vec<ProviderConfig> {
    let mut ordered = Vec::new();
    for id in &group.provider_order {
        if let Some(provider) = config.providers.get(id) {
            if provider.enabled {
                ordered.push(provider.clone());
            }
        }
    }

    if matches!(group.policy, GroupPolicy::Manual) {
        ordered.truncate(1);
    }
    ordered
}

pub fn filter_available_providers(
    providers: Vec<ProviderConfig>,
    health: &BTreeMap<String, ProviderHealth>,
) -> Vec<ProviderConfig> {
    providers
        .into_iter()
        .filter(|provider| {
            !matches!(
                health.get(&provider.id).map(|state| &state.status),
                Some(HealthStatusKind::Cooldown | HealthStatusKind::AuthFailed)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{default_refresh_interval_seconds, ProviderKind, RelayConfig};

    fn provider(id: &str, priority: i32) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    #[test]
    fn honors_group_provider_order() {
        let mut config = CompanionConfig {
            relay: RelayConfig {
                active_group_id: "main".to_string(),
                ..RelayConfig::default()
            },
            ..CompanionConfig::default()
        };
        config.providers.insert("a".to_string(), provider("a", 20));
        config.providers.insert("b".to_string(), provider("b", 10));
        config.groups.insert(
            "main".to_string(),
            ProviderGroup {
                id: "main".to_string(),
                name: "Main".to_string(),
                policy: GroupPolicy::PriorityFallback,
                provider_order: vec!["b".to_string(), "a".to_string()],
                provider_weights: BTreeMap::new(),
                fallback_enabled: true,
            },
        );

        let ids: Vec<_> = selected_providers(&config)
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn empty_group_has_no_selected_providers() {
        let mut config = CompanionConfig::default();
        config.providers.insert("a".to_string(), provider("a", 20));
        config.providers.insert("b".to_string(), provider("b", 10));

        let ids: Vec<_> = selected_providers(&config)
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        assert!(ids.is_empty());
    }

    #[test]
    fn manual_group_uses_first_provider_only() {
        let mut config = CompanionConfig::default();
        config.relay.active_group_id = "manual".to_string();
        config.providers.insert("a".to_string(), provider("a", 10));
        config.providers.insert("b".to_string(), provider("b", 20));
        config.groups.insert(
            "manual".to_string(),
            ProviderGroup {
                id: "manual".to_string(),
                name: "Manual".to_string(),
                policy: GroupPolicy::Manual,
                provider_order: vec!["b".to_string(), "a".to_string()],
                provider_weights: BTreeMap::new(),
                fallback_enabled: false,
            },
        );

        let ids: Vec<_> = selected_providers(&config)
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        assert_eq!(ids, vec!["b"]);
    }
}
