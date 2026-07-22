use crate::api_service::ApiServiceStore;
use codex_companion_core::ConfigStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
}

fn prune_bindings(bindings: &mut HashMap<String, SessionAffinityBinding>, ttl_seconds: u64) {
    let now = Instant::now();
    let ttl = Duration::from_secs(ttl_seconds.clamp(60, 86_400));
    bindings.retain(|_, binding| now.duration_since(binding.updated_at) <= ttl);
}
