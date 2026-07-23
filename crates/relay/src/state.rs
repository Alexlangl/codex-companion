use crate::api_service::ApiServiceStore;
use codex_companion_core::{ConfigStore, GroupPolicy, ProviderConfig, ProviderGroup};
use rand::{seq::SliceRandom, Rng};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

const MAX_SESSION_AFFINITY_BINDINGS: usize = 4096;

#[derive(Debug, Clone)]
struct SessionAffinityBinding {
    provider_id: String,
    updated_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct RelayState {
    pub store: ConfigStore,
    pub client: reqwest::Client,
    pub api_service: ApiServiceStore,
    session_affinity: Arc<Mutex<HashMap<String, SessionAffinityBinding>>>,
    provider_inflight: Arc<Mutex<HashMap<String, usize>>>,
    round_robin_sequence: Arc<AtomicU64>,
}

pub(crate) struct ProviderRequestGuard {
    provider_id: String,
    provider_inflight: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for ProviderRequestGuard {
    fn drop(&mut self) {
        let Ok(mut inflight) = self.provider_inflight.lock() else {
            return;
        };
        if let Some(count) = inflight.get_mut(&self.provider_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inflight.remove(&self.provider_id);
            }
        }
    }
}

impl RelayState {
    pub(crate) fn new(store: ConfigStore, client: reqwest::Client) -> Self {
        let api_service = ApiServiceStore::from_config_store(&store);
        let _ = api_service.initialize();
        Self {
            store,
            client,
            api_service,
            session_affinity: Arc::new(Mutex::new(HashMap::new())),
            provider_inflight: Arc::new(Mutex::new(HashMap::new())),
            round_robin_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn preferred_provider(&self, key: &str, ttl_seconds: u64) -> Option<String> {
        let mut bindings = self.session_affinity.lock().ok()?;
        prune_bindings(&mut bindings, ttl_seconds);
        if let Some(binding) = bindings.get_mut(key) {
            binding.updated_at = Instant::now();
            return Some(binding.provider_id.clone());
        }
        drop(bindings);
        self.api_service
            .preferred_affinity(key, ttl_seconds)
            .ok()
            .flatten()
    }

    pub(crate) fn bind_provider(&self, key: &str, provider_id: &str, ttl_seconds: u64) {
        let Ok(mut bindings) = self.session_affinity.lock() else {
            return;
        };
        prune_bindings(&mut bindings, ttl_seconds);
        bindings.insert(
            key.to_string(),
            SessionAffinityBinding {
                provider_id: provider_id.to_string(),
                updated_at: Instant::now(),
            },
        );
        let _ = self.api_service.bind_affinity(key, provider_id);
        if bindings.len() > MAX_SESSION_AFFINITY_BINDINGS {
            if let Some(oldest) = bindings
                .iter()
                .min_by_key(|(_, binding)| binding.updated_at)
                .map(|(key, _)| key.clone())
            {
                bindings.remove(&oldest);
            }
        }
    }

    pub(crate) fn next_round_robin_index(&self, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        (self.round_robin_sequence.fetch_add(1, Ordering::Relaxed) as usize) % len
    }

    pub(crate) fn provider_inflight_count(&self, provider_id: &str) -> usize {
        self.provider_inflight
            .lock()
            .ok()
            .and_then(|inflight| inflight.get(provider_id).copied())
            .unwrap_or_default()
    }

    pub(crate) fn begin_provider_request(&self, provider_id: &str) -> ProviderRequestGuard {
        if let Ok(mut inflight) = self.provider_inflight.lock() {
            *inflight.entry(provider_id.to_string()).or_default() += 1;
        }
        ProviderRequestGuard {
            provider_id: provider_id.to_string(),
            provider_inflight: self.provider_inflight.clone(),
        }
    }
}

pub(crate) fn apply_group_policy(
    state: &RelayState,
    group: &ProviderGroup,
    candidates: &mut Vec<ProviderConfig>,
) {
    let mut rng = rand::rng();
    apply_group_policy_with_rng(state, group, candidates, &mut rng);
}

fn apply_group_policy_with_rng<R: Rng + ?Sized>(
    state: &RelayState,
    group: &ProviderGroup,
    candidates: &mut Vec<ProviderConfig>,
    rng: &mut R,
) {
    if candidates.len() <= 1 {
        return;
    }
    match group.policy {
        GroupPolicy::PriorityFallback | GroupPolicy::Manual => {}
        GroupPolicy::RoundRobin => {
            let index = state.next_round_robin_index(candidates.len());
            candidates.rotate_left(index);
        }
        GroupPolicy::Random => candidates.shuffle(rng),
        GroupPolicy::Weighted => {
            let total_weight = candidates
                .iter()
                .map(|provider| provider_weight(group, &provider.id))
                .sum::<u64>();
            let selected_weight = rng.random_range(0..total_weight.max(1));
            let selected_index = weighted_candidate_index(group, candidates, selected_weight);
            candidates.rotate_left(selected_index);
        }
        GroupPolicy::LeastLoaded => {
            candidates.sort_by_key(|provider| state.provider_inflight_count(&provider.id));
        }
    }
}

fn weighted_candidate_index(
    group: &ProviderGroup,
    candidates: &[ProviderConfig],
    mut selected_weight: u64,
) -> usize {
    candidates
        .iter()
        .position(|provider| {
            let weight = provider_weight(group, &provider.id);
            if selected_weight < weight {
                true
            } else {
                selected_weight -= weight;
                false
            }
        })
        .unwrap_or_default()
}

fn provider_weight(group: &ProviderGroup, provider_id: &str) -> u64 {
    u64::from(
        group
            .provider_weights
            .get(provider_id)
            .copied()
            .unwrap_or(1)
            .max(1),
    )
}

fn prune_bindings(bindings: &mut HashMap<String, SessionAffinityBinding>, ttl_seconds: u64) {
    let now = Instant::now();
    let ttl = Duration::from_secs(ttl_seconds.clamp(60, 86_400));
    bindings.retain(|_, binding| now.duration_since(binding.updated_at) <= ttl);
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{default_refresh_interval_seconds, ProviderKind};
    use rand::{rngs::StdRng, SeedableRng};
    use std::collections::{BTreeMap, BTreeSet};

    fn state() -> RelayState {
        let temp = tempfile::tempdir().expect("temp");
        let data_dir = temp.keep();
        RelayState::new(
            ConfigStore::new(data_dir.join("config.json")),
            reqwest::Client::new(),
        )
    }

    fn provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        }
    }

    fn group(policy: GroupPolicy) -> ProviderGroup {
        ProviderGroup {
            id: "test".to_string(),
            name: "Test".to_string(),
            policy,
            provider_order: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            provider_weights: BTreeMap::new(),
            fallback_enabled: true,
        }
    }

    fn ids(candidates: &[ProviderConfig]) -> Vec<&str> {
        candidates
            .iter()
            .map(|provider| provider.id.as_str())
            .collect()
    }

    #[test]
    fn round_robin_rotates_the_first_candidate_between_requests() {
        let state = state();
        let group = group(GroupPolicy::RoundRobin);

        let mut first = vec![provider("a"), provider("b"), provider("c")];
        apply_group_policy(&state, &group, &mut first);
        let mut second = vec![provider("a"), provider("b"), provider("c")];
        apply_group_policy(&state, &group, &mut second);
        let mut third = vec![provider("a"), provider("b"), provider("c")];
        apply_group_policy(&state, &group, &mut third);

        assert_eq!(ids(&first), vec!["a", "b", "c"]);
        assert_eq!(ids(&second), vec!["b", "c", "a"]);
        assert_eq!(ids(&third), vec!["c", "a", "b"]);
    }

    #[test]
    fn random_policy_is_repeatable_with_a_seed_and_preserves_candidates() {
        let state = state();
        let group = group(GroupPolicy::Random);
        let original = vec![provider("a"), provider("b"), provider("c")];
        let mut first = original.clone();
        let mut second = original.clone();
        let mut first_rng = StdRng::seed_from_u64(42);
        let mut second_rng = StdRng::seed_from_u64(42);

        apply_group_policy_with_rng(&state, &group, &mut first, &mut first_rng);
        apply_group_policy_with_rng(&state, &group, &mut second, &mut second_rng);

        assert_eq!(ids(&first), ids(&second));
        assert_ne!(ids(&first), ids(&original));
        assert_eq!(
            ids(&first).into_iter().collect::<BTreeSet<_>>(),
            ids(&original).into_iter().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn weighted_policy_maps_each_roll_to_the_expected_provider() {
        let mut group = group(GroupPolicy::Weighted);
        group.provider_weights.insert("a".to_string(), 1);
        group.provider_weights.insert("b".to_string(), 3);
        group.provider_weights.insert("c".to_string(), 2);
        let candidates = vec![provider("a"), provider("b"), provider("c")];

        assert_eq!(weighted_candidate_index(&group, &candidates, 0), 0);
        assert_eq!(weighted_candidate_index(&group, &candidates, 1), 1);
        assert_eq!(weighted_candidate_index(&group, &candidates, 3), 1);
        assert_eq!(weighted_candidate_index(&group, &candidates, 4), 2);
        assert_eq!(weighted_candidate_index(&group, &candidates, 5), 2);
    }

    #[test]
    fn least_loaded_orders_idle_providers_first() {
        let state = state();
        let group = group(GroupPolicy::LeastLoaded);
        let _a_first = state.begin_provider_request("a");
        let _a_second = state.begin_provider_request("a");
        let _b = state.begin_provider_request("b");
        let mut candidates = vec![provider("a"), provider("b"), provider("c")];

        apply_group_policy(&state, &group, &mut candidates);

        assert_eq!(ids(&candidates), vec!["c", "b", "a"]);
    }
}
