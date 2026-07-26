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

pub(crate) struct RefreshProgressGuard {
    store: ConfigStore,
    finished: bool,
}

impl RefreshProgressGuard {
    pub(crate) fn begin(store: &ConfigStore, ids: &[String]) -> Self {
        begin_refresh(store, ids);
        Self {
            store: store.clone(),
            finished: false,
        }
    }

    pub(crate) fn mark_provider(&self, provider_id: &str, completed: usize) {
        mark_refresh_provider(&self.store, provider_id, completed);
    }

    pub(crate) fn finish(mut self, error: Option<String>) {
        finish_refresh(&self.store, error);
        self.finished = true;
    }
}

impl Drop for RefreshProgressGuard {
    fn drop(&mut self) {
        if !self.finished {
            finish_refresh(&self.store, Some("Provider 刷新已取消".to_string()));
        }
    }
}

pub(crate) struct HealthRefreshLoop {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HealthRefreshLoop {
    pub(crate) async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }

    fn detach(mut self) {
        self.task.take();
    }
}

impl Drop for HealthRefreshLoop {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct LoopRegistration {
    data_dir: PathBuf,
}

impl LoopRegistration {
    fn acquire(data_dir: PathBuf) -> Option<Self> {
        let started = STARTED_LOOPS.get_or_init(|| Mutex::new(HashSet::new()));
        let mut started = started.lock().expect("health refresh loop mutex poisoned");
        started
            .insert(data_dir.clone())
            .then_some(Self { data_dir })
    }
}

impl Drop for LoopRegistration {
    fn drop(&mut self) {
        if let Ok(mut started) = STARTED_LOOPS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
        {
            started.remove(&self.data_dir);
        }
    }
}

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

fn begin_refresh(store: &ConfigStore, ids: &[String]) {
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

fn mark_refresh_provider(store: &ConfigStore, provider_id: &str, completed: usize) {
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

fn finish_refresh(store: &ConfigStore, error: Option<String>) {
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
    let Some(refresh_loop) = start_scoped_health_refresh_loop(store) else {
        return false;
    };
    refresh_loop.detach();
    true
}

pub(crate) fn start_scoped_health_refresh_loop(store: ConfigStore) -> Option<HealthRefreshLoop> {
    let data_dir = store.data_dir();
    let registration = LoopRegistration::acquire(data_dir)?;

    let task = tokio::spawn(async move {
        let _registration = registration;
        loop {
            refresh_due_providers(&store).await;
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
    Some(HealthRefreshLoop { task: Some(task) })
}

#[cfg(test)]
pub(crate) fn health_refresh_loop_started(store: &ConfigStore) -> bool {
    STARTED_LOOPS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .expect("health refresh loop mutex poisoned")
        .contains(&store.data_dir())
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
    let progress = RefreshProgressGuard::begin(store, &ids);
    let mut first_error = None;
    for (index, id) in ids.iter().enumerate() {
        progress.mark_provider(id, index);
        if let Err(error) = refresh_provider_status(store, id).await {
            first_error.get_or_insert_with(|| error.to_string());
        }
        progress.mark_provider(id, index + 1);
    }
    progress.finish(first_error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_loop_unregisters_when_stopped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let refresh_loop =
            start_scoped_health_refresh_loop(store.clone()).expect("start refresh loop");
        assert!(health_refresh_loop_started(&store));

        refresh_loop.stop().await;

        assert!(!health_refresh_loop_started(&store));
    }

    #[tokio::test]
    async fn cancelled_refresh_marks_progress_inactive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let task_store = store.clone();
        let task = tokio::spawn(async move {
            let _progress = RefreshProgressGuard::begin(&task_store, &["provider-a".to_string()]);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        assert!(refresh_progress(&store).active);

        task.abort();
        let _ = task.await;

        let progress = refresh_progress(&store);
        assert!(!progress.active);
        assert_eq!(progress.last_error.as_deref(), Some("Provider 刷新已取消"));
    }
}
