use chrono::Utc;
use codex_companion_core::{ConfigStore, ProviderRefreshProgress};
use codex_companion_provider::refresh_provider_status;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static STARTED_LOOPS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static REFRESH_PROGRESS: OnceLock<Mutex<HashMap<PathBuf, ProviderRefreshProgress>>> =
    OnceLock::new();
static REFRESH_COORDINATOR: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(crate) fn refresh_coordinator() -> &'static tokio::sync::Mutex<()> {
    REFRESH_COORDINATOR.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(crate) fn refresh_progress(store: &ConfigStore) -> ProviderRefreshProgress {
    REFRESH_PROGRESS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|progress| progress.get(&store.data_dir()).cloned())
        .unwrap_or_default()
}

pub(crate) fn begin_refresh(store: &ConfigStore, ids: &[String]) {
    if let Ok(mut progress) = REFRESH_PROGRESS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        progress.insert(
            store.data_dir(),
            ProviderRefreshProgress {
                active: true,
                total: ids.len(),
                started_at: Some(Utc::now()),
                ..ProviderRefreshProgress::default()
            },
        );
    }
}

pub(crate) fn mark_refresh_provider(store: &ConfigStore, provider_id: &str, completed: usize) {
    if let Ok(mut progress) = REFRESH_PROGRESS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        if let Some(current) = progress.get_mut(&store.data_dir()) {
            current.current_provider_id = Some(provider_id.to_string());
            current.completed = completed;
        }
    }
}

pub(crate) fn finish_refresh(store: &ConfigStore, error: Option<String>) {
    if let Ok(mut progress) = REFRESH_PROGRESS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        if let Some(current) = progress.get_mut(&store.data_dir()) {
            current.active = false;
            current.current_provider_id = None;
            current.finished_at = Some(Utc::now());
            current.last_error = error;
        }
    }
}

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
    let _guard = refresh_coordinator().lock().await;
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

    if ids.is_empty() {
        return;
    }
    begin_refresh(store, &ids);
    let mut first_error = None;
    for (index, id) in ids.iter().enumerate() {
        mark_refresh_provider(store, id, index);
        if let Err(error) = refresh_provider_status(store, id).await {
            first_error.get_or_insert_with(|| error.to_string());
        }
        mark_refresh_provider(store, id, index + 1);
    }
    finish_refresh(store, first_error);
}
