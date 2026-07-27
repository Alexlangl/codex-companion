use crate::types::GroupUpsert;
use crate::validate::validate_id;
use codex_companion_core::{
    CompanionConfig, CompanionError, ConfigStore, GroupPolicy, HealthStatusKind, ProviderConfig,
    ProviderGroup, ProviderHealth, Result,
};
use std::collections::BTreeMap;

pub fn upsert_group(store: &ConfigStore, input: GroupUpsert) -> Result<ProviderGroup> {
    validate_id(&input.id)?;
    validate_priority_failback_interval(&input)?;
    store.update(|config| {
        for provider_id in &input.provider_order {
            if !config.providers.contains_key(provider_id) {
                return Err(CompanionError::InvalidConfig(format!(
                    "unknown provider in group: {provider_id}"
                )));
            }
        }
        let group_id = input.id.trim().to_string();
        let priority_failback_revision = config
            .groups
            .get(&group_id)
            .map(|group| group.priority_failback_revision)
            .unwrap_or_default();
        let priority_failback_target_provider_id = config
            .groups
            .get(&group_id)
            .and_then(|group| group.priority_failback_target_provider_id.clone())
            .filter(|provider_id| input.provider_order.contains(provider_id));
        let group = ProviderGroup {
            id: group_id,
            name: input.name.trim().to_string(),
            policy: input.policy,
            provider_order: input.provider_order,
            provider_weights: input.provider_weights,
            fallback_enabled: input.fallback_enabled,
            priority_failback_interval_seconds: input.priority_failback_interval_seconds,
            priority_failback_revision,
            priority_failback_target_provider_id,
        };
        config.groups.insert(group.id.clone(), group.clone());
        Ok(group)
    })
}

pub fn request_priority_failback(
    store: &ConfigStore,
    id: &str,
    provider_id: &str,
) -> Result<ProviderGroup> {
    store.update(|config| {
        let group = config
            .groups
            .get(id)
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {id}")))?;
        if !matches!(group.policy, GroupPolicy::PriorityFallback) || !group.fallback_enabled {
            return Err(CompanionError::InvalidConfig(
                "只有已启用故障切换的优先级分组可以尝试上一级 Provider".into(),
            ));
        }
        if group.provider_order.len() < 2 {
            return Err(CompanionError::InvalidConfig(
                "分组至少需要两个 Provider 才能尝试上一级".into(),
            ));
        }
        if !group
            .provider_order
            .iter()
            .any(|candidate_id| candidate_id == provider_id)
        {
            return Err(CompanionError::InvalidConfig(format!(
                "Provider {provider_id} 不在分组 {id} 中"
            )));
        }
        let provider = config.providers.get(provider_id).ok_or_else(|| {
            CompanionError::InvalidConfig(format!("unknown provider in group: {provider_id}"))
        })?;
        if !provider.enabled {
            return Err(CompanionError::InvalidConfig(format!(
                "Provider {provider_id} 已禁用，无法手动尝试"
            )));
        }
        let group = config
            .groups
            .get_mut(id)
            .ok_or_else(|| CompanionError::InvalidConfig(format!("unknown group: {id}")))?;
        group.priority_failback_revision = group.priority_failback_revision.wrapping_add(1);
        group.priority_failback_target_provider_id = Some(provider_id.to_string());
        Ok(group.clone())
    })
}

fn validate_priority_failback_interval(input: &GroupUpsert) -> Result<()> {
    let interval = input.priority_failback_interval_seconds;
    if interval == 0 {
        return Ok(());
    }
    if !matches!(input.policy, GroupPolicy::PriorityFallback) || !input.fallback_enabled {
        return Err(CompanionError::InvalidConfig(
            "自动向上探测只适用于已启用故障切换的优先级分组".into(),
        ));
    }
    if !(10..=3_600).contains(&interval) {
        return Err(CompanionError::InvalidConfig(
            "自动向上探测间隔必须为 10 到 3600 秒，或设为 0 关闭".into(),
        ));
    }
    Ok(())
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
        if group
            .priority_failback_target_provider_id
            .as_ref()
            .is_some_and(|provider_id| !group.provider_order.contains(provider_id))
        {
            group.priority_failback_target_provider_id = None;
        }
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
                priority_failback_interval_seconds: 0,
                priority_failback_revision: 0,
                priority_failback_target_provider_id: None,
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
                priority_failback_interval_seconds: 0,
                priority_failback_revision: 0,
                priority_failback_target_provider_id: None,
            },
        );

        let ids: Vec<_> = selected_providers(&config)
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn manual_failback_request_is_preserved_when_group_is_edited() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert("a".to_string(), provider("a", 10));
                config.providers.insert("b".to_string(), provider("b", 20));
                Ok(())
            })
            .expect("providers");
        let input = GroupUpsert {
            id: "work".to_string(),
            name: "Work".to_string(),
            policy: GroupPolicy::PriorityFallback,
            provider_order: vec!["a".to_string(), "b".to_string()],
            provider_weights: BTreeMap::new(),
            fallback_enabled: true,
            priority_failback_interval_seconds: 60,
        };

        let created = upsert_group(&store, input.clone()).expect("create group");
        assert_eq!(created.priority_failback_revision, 0);
        assert!(request_priority_failback(&store, "work", "missing").is_err());
        let requested = request_priority_failback(&store, "work", "a").expect("request failback");
        assert_eq!(requested.priority_failback_revision, 1);
        assert_eq!(
            requested.priority_failback_target_provider_id.as_deref(),
            Some("a")
        );
        let edited = upsert_group(
            &store,
            GroupUpsert {
                name: "Renamed".to_string(),
                ..input
            },
        )
        .expect("edit group");
        assert_eq!(edited.priority_failback_revision, 1);
    }

    #[test]
    fn manual_failback_rejects_a_disabled_provider() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert("a".to_string(), provider("a", 10));
                let mut disabled = provider("b", 20);
                disabled.enabled = false;
                config.providers.insert("b".to_string(), disabled);
                Ok(())
            })
            .expect("providers");
        upsert_group(
            &store,
            GroupUpsert {
                id: "work".to_string(),
                name: "Work".to_string(),
                policy: GroupPolicy::PriorityFallback,
                provider_order: vec!["a".to_string(), "b".to_string()],
                provider_weights: BTreeMap::new(),
                fallback_enabled: true,
                priority_failback_interval_seconds: 0,
            },
        )
        .expect("create group");

        assert!(request_priority_failback(&store, "work", "b").is_err());
        let group = store
            .load()
            .expect("config")
            .groups
            .remove("work")
            .expect("group");
        assert_eq!(group.priority_failback_revision, 0);
        assert_eq!(group.priority_failback_target_provider_id, None);
    }

    #[test]
    fn changing_group_order_clears_a_removed_failback_target() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.providers.insert("a".to_string(), provider("a", 10));
                config.providers.insert("b".to_string(), provider("b", 20));
                Ok(())
            })
            .expect("providers");
        upsert_group(
            &store,
            GroupUpsert {
                id: "work".to_string(),
                name: "Work".to_string(),
                policy: GroupPolicy::PriorityFallback,
                provider_order: vec!["a".to_string(), "b".to_string()],
                provider_weights: BTreeMap::new(),
                fallback_enabled: true,
                priority_failback_interval_seconds: 0,
            },
        )
        .expect("create group");
        request_priority_failback(&store, "work", "a").expect("request failback");

        let group = set_group_order(&store, "work", vec!["b".to_string()]).expect("reorder");
        assert_eq!(group.priority_failback_target_provider_id, None);
    }

    #[test]
    fn priority_failback_interval_is_opt_in_and_bounded() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let invalid = GroupUpsert {
            id: "work".to_string(),
            name: "Work".to_string(),
            policy: GroupPolicy::PriorityFallback,
            provider_order: Vec::new(),
            provider_weights: BTreeMap::new(),
            fallback_enabled: true,
            priority_failback_interval_seconds: 9,
        };

        assert!(upsert_group(&store, invalid).is_err());
    }
}
