use codex_companion_core::ConfigStore;
use codex_companion_provider::refresh_provider_status;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static STARTED_LOOPS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub fn start_health_refresh_loop(store: ConfigStore) -> bool {
    let data_dir = store.data_dir();
    let started = STARTED_LOOPS.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let mut started = started.lock().expect("health refresh loop mutex poisoned");
        if !started.insert(data_dir) {
            return false;
        }
    }

    tokio::spawn(async move {
        loop {
            refresh_due_providers(&store).await;
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
    true
}

async fn refresh_due_providers(store: &ConfigStore) {
    let Ok(config) = store.load() else {
        return;
    };
    let now = chrono::Utc::now();
    let ids = config
        .providers
        .values()
        .filter(|provider| provider.enabled)
        .filter(|provider| {
            config
                .health
                .get(&provider.id)
                .and_then(|health| health.last_checked)
                .is_none_or(|last_checked| {
                    now.signed_duration_since(last_checked).num_seconds()
                        >= provider.refresh_interval_seconds as i64
                })
        })
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();

    for id in ids {
        let _ = refresh_provider_status(store, &id).await;
    }
}
