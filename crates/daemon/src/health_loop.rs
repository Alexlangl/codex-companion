use chrono::{Duration as ChronoDuration, Utc};
use codex_companion_core::{
    append_diagnostic_log, CompanionConfig, ConfigStore, ProviderConfig, ProviderHealth,
    ProviderKind, ProviderRefreshProgress,
};
use codex_companion_health::{mark_failure, mark_success};
use codex_companion_provider::{
    ensure_codex_auth_snapshot_detailed, provider_uses_codex_oauth, refresh_provider_status,
};
use codex_companion_state::sync_managed_official_oauth_auth;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static STARTED_LOOPS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static REFRESH_PROGRESS: OnceLock<Mutex<HashMap<PathBuf, ProviderRefreshProgress>>> =
    OnceLock::new();
static REFRESH_COORDINATOR: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static OAUTH_KEEPALIVE_ERRORS: OnceLock<Mutex<HashMap<(PathBuf, String), String>>> =
    OnceLock::new();
static OAUTH_KEEPALIVE_BACKOFF: OnceLock<Mutex<HashMap<(PathBuf, String), OAuthKeepaliveBackoff>>> =
    OnceLock::new();
static OAUTH_NATIVE_AUTH_MIRROR_ERRORS: OnceLock<Mutex<HashMap<(PathBuf, String), String>>> =
    OnceLock::new();

const OAUTH_KEEPALIVE_MAX_BACKOFF_SECONDS: i64 = 15 * 60;

#[derive(Debug, Clone, Copy, Default)]
struct OAuthKeepaliveBackoff {
    failures: u32,
    next_attempt: Option<chrono::DateTime<Utc>>,
}

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
    let Ok(config) = store.load() else {
        return;
    };
    keep_official_oauth_alive(store, &config).await;
    // Keep the OAuth token endpoint outside the global health-refresh lock.
    // The auth file has its own per-file flock, so this no longer blocks a
    // manual refresh or another provider for the whole network timeout.
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
            provider_refresh_due(
                provider.refresh_interval_seconds,
                config.health.get(&provider.id),
                now,
            )
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

fn official_oauth_keepalive_providers(config: &CompanionConfig) -> Vec<ProviderConfig> {
    config
        .providers
        .values()
        .filter(|provider| {
            provider.enabled
                && provider.kind == ProviderKind::OfficialCodex
                && provider_uses_codex_oauth(provider)
        })
        .cloned()
        .collect()
}

async fn keep_official_oauth_alive(store: &ConfigStore, config: &CompanionConfig) {
    let now = Utc::now();
    for provider in official_oauth_keepalive_providers(config) {
        if !oauth_keepalive_is_due(store, &provider.id, now) {
            continue;
        }
        match ensure_codex_auth_snapshot_detailed(&provider).await {
            Ok(_) => {
                let Some(current) = current_provider_if_unchanged(store, &provider) else {
                    continue;
                };
                sync_managed_oauth_auth(store, &current);
                clear_oauth_keepalive_error(store, &current.id);
                clear_oauth_keepalive_health_failure(store, &current.id);
            }
            Err(error) => {
                if current_provider_if_unchanged(store, &provider).is_none() {
                    continue;
                }
                record_oauth_keepalive_failure(store, &provider.id, now);
                let message = error.to_string();
                persist_oauth_keepalive_failure(
                    store,
                    &provider.id,
                    &error.failure_classification(),
                    &message,
                );
                if should_log_oauth_keepalive_error(store, &provider.id, &message) {
                    let _ = append_diagnostic_log(
                        &store.data_dir(),
                        "warn",
                        "oauth",
                        &format!("Provider {} OAuth 保活失败: {message}", provider.id),
                    );
                }
            }
        }
    }
}

fn current_provider_if_unchanged(
    store: &ConfigStore,
    expected: &ProviderConfig,
) -> Option<ProviderConfig> {
    let config = store.load().ok()?;
    let current = config.providers.get(&expected.id)?;
    (current == expected).then(|| current.clone())
}

fn sync_managed_oauth_auth(store: &ConfigStore, provider: &ProviderConfig) {
    match sync_managed_official_oauth_auth(None, provider) {
        Ok(_) => clear_oauth_native_auth_mirror_error(store, &provider.id),
        Err(error) => {
            let message = error.to_string();
            if should_log_oauth_native_auth_mirror_error(store, &provider.id, &message) {
                let _ = append_diagnostic_log(
                    &store.data_dir(),
                    "warn",
                    "oauth",
                    &format!(
                        "Provider {} OAuth 已刷新，但未同步受管 Codex auth.json: {message}",
                        provider.id
                    ),
                );
            }
        }
    }
}

fn persist_oauth_keepalive_failure(
    store: &ConfigStore,
    provider_id: &str,
    failure: &codex_companion_health::FailureClassification,
    message: &str,
) {
    let detail = format!("OAuth 保活失败: {message}");
    let _ = store.update(|config| {
        let health = config.health.entry(provider_id.to_string()).or_default();
        mark_failure(health, failure, detail);
        Ok(())
    });
}

fn clear_oauth_keepalive_health_failure(store: &ConfigStore, provider_id: &str) {
    let _ = store.update(|config| {
        let Some(health) = config.health.get_mut(provider_id) else {
            return Ok(());
        };
        let is_keepalive_failure = health
            .last_error
            .as_deref()
            .is_some_and(|message| message.starts_with("OAuth 保活失败: "));
        if is_keepalive_failure {
            mark_success(health);
        }
        Ok(())
    });
}

fn should_log_oauth_keepalive_error(store: &ConfigStore, provider_id: &str, message: &str) -> bool {
    let key = (store.data_dir(), provider_id.to_string());
    let errors = OAUTH_KEEPALIVE_ERRORS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut errors) = errors.lock() else {
        return true;
    };
    if errors.get(&key).is_some_and(|previous| previous == message) {
        return false;
    }
    errors.insert(key, message.to_string());
    true
}

fn clear_oauth_keepalive_error(store: &ConfigStore, provider_id: &str) {
    let key = (store.data_dir(), provider_id.to_string());
    if let Ok(mut errors) = OAUTH_KEEPALIVE_ERRORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        errors.remove(&key);
    }
    if let Ok(mut backoff) = OAUTH_KEEPALIVE_BACKOFF
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        backoff.remove(&key);
    }
}

fn should_log_oauth_native_auth_mirror_error(
    store: &ConfigStore,
    provider_id: &str,
    message: &str,
) -> bool {
    let key = (store.data_dir(), provider_id.to_string());
    let errors = OAUTH_NATIVE_AUTH_MIRROR_ERRORS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut errors) = errors.lock() else {
        return true;
    };
    if errors.get(&key).is_some_and(|previous| previous == message) {
        return false;
    }
    errors.insert(key, message.to_string());
    true
}

fn clear_oauth_native_auth_mirror_error(store: &ConfigStore, provider_id: &str) {
    let key = (store.data_dir(), provider_id.to_string());
    if let Ok(mut errors) = OAUTH_NATIVE_AUTH_MIRROR_ERRORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        errors.remove(&key);
    }
}

fn oauth_keepalive_is_due(
    store: &ConfigStore,
    provider_id: &str,
    now: chrono::DateTime<Utc>,
) -> bool {
    let key = (store.data_dir(), provider_id.to_string());
    OAUTH_KEEPALIVE_BACKOFF
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|backoff| backoff.get(&key).copied())
        .and_then(|state| state.next_attempt)
        .is_none_or(|next_attempt| next_attempt <= now)
}

fn record_oauth_keepalive_failure(
    store: &ConfigStore,
    provider_id: &str,
    now: chrono::DateTime<Utc>,
) {
    let key = (store.data_dir(), provider_id.to_string());
    let Ok(mut backoff) = OAUTH_KEEPALIVE_BACKOFF
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return;
    };
    let state = backoff.entry(key).or_default();
    state.failures = state.failures.saturating_add(1);
    let exponent = state.failures.saturating_sub(1).min(6);
    let seconds = 15_i64
        .saturating_mul(1_i64 << exponent)
        .min(OAUTH_KEEPALIVE_MAX_BACKOFF_SECONDS);
    state.next_attempt = Some(now + ChronoDuration::seconds(seconds));
}

fn provider_refresh_due(
    refresh_interval_seconds: u64,
    health: Option<&ProviderHealth>,
    now: chrono::DateTime<Utc>,
) -> bool {
    health
        .and_then(|health| health.last_refresh_attempt)
        .is_none_or(|last_attempt| {
            now.signed_duration_since(last_attempt).num_seconds() >= refresh_interval_seconds as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

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

    #[test]
    fn relay_health_success_does_not_postpone_scheduled_account_refresh() {
        let now = Utc::now();
        let mut health = ProviderHealth {
            last_checked: Some(now),
            last_refresh_attempt: Some(now - ChronoDuration::seconds(61)),
            ..ProviderHealth::default()
        };

        assert!(provider_refresh_due(60, Some(&health), now));

        health.last_refresh_attempt = Some(now);
        assert!(!provider_refresh_due(60, Some(&health), now));
        assert!(provider_refresh_due(60, None, now));
    }

    #[test]
    fn official_oauth_keepalive_is_selected_independently_of_health_interval() {
        let mut config = CompanionConfig::default();
        config.providers.insert(
            "official".to_string(),
            ProviderConfig {
                id: "official".to_string(),
                name: "Official".to_string(),
                kind: ProviderKind::OfficialCodex,
                base_url: "https://chatgpt.com/backend-api/codex".to_string(),
                websocket_url: None,
                auth_ref: Some("file:/tmp/official-auth.json".to_string()),
                direct_auth_ref: None,
                model_map: Default::default(),
                priority: 0,
                enabled: true,
                refresh_interval_seconds: 24 * 60 * 60,
                account: None,
            },
        );

        let providers = official_oauth_keepalive_providers(&config);

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].refresh_interval_seconds, 24 * 60 * 60);
    }

    #[test]
    fn oauth_keepalive_failures_back_off_and_success_clears_backoff() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let now = Utc::now();

        assert!(oauth_keepalive_is_due(&store, "official", now));
        record_oauth_keepalive_failure(&store, "official", now);
        assert!(!oauth_keepalive_is_due(&store, "official", now));
        assert!(oauth_keepalive_is_due(
            &store,
            "official",
            now + ChronoDuration::seconds(15)
        ));

        clear_oauth_keepalive_error(&store, "official");
        assert!(oauth_keepalive_is_due(&store, "official", now));
    }

    #[test]
    fn stale_keepalive_result_cannot_touch_a_changed_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let mut config = CompanionConfig::default();
        let provider = ProviderConfig {
            id: "official".to_string(),
            name: "Official".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            websocket_url: None,
            auth_ref: Some("file:/tmp/official-auth.json".to_string()),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: 900,
            account: None,
        };
        config
            .providers
            .insert(provider.id.clone(), provider.clone());
        store.save(&config).expect("save provider");

        let mut changed = provider;
        changed.auth_ref = Some("file:/tmp/replaced-auth.json".to_string());
        store
            .update(|config| {
                config.providers.insert("official".to_string(), changed);
                Ok(())
            })
            .expect("replace provider");

        assert!(current_provider_if_unchanged(
            &store,
            config.providers.get("official").expect("original provider")
        )
        .is_none());
    }

    #[test]
    fn keepalive_failure_is_persisted_and_only_its_own_recovery_clears_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let failure = codex_companion_health::classification_for_kind(
            codex_companion_core::HealthFailureKind::AuthFailed,
        );

        persist_oauth_keepalive_failure(&store, "official", &failure, "refresh token revoked");

        let failed = store.load().expect("failed config");
        assert_eq!(
            failed.health["official"].status,
            codex_companion_core::HealthStatusKind::AuthFailed
        );
        assert_eq!(
            failed.health["official"].last_error.as_deref(),
            Some("OAuth 保活失败: refresh token revoked")
        );

        clear_oauth_keepalive_health_failure(&store, "official");

        let recovered = store.load().expect("recovered config");
        assert_eq!(
            recovered.health["official"].status,
            codex_companion_core::HealthStatusKind::Healthy
        );
        assert!(recovered.health["official"].last_error.is_none());

        store
            .update(|config| {
                config.health.insert(
                    "official".to_string(),
                    ProviderHealth {
                        last_error: Some("provider request failed".to_string()),
                        ..ProviderHealth::default()
                    },
                );
                Ok(())
            })
            .expect("unrelated failure");
        clear_oauth_keepalive_health_failure(&store, "official");
        assert_eq!(
            store.load().expect("unchanged config").health["official"]
                .last_error
                .as_deref(),
            Some("provider request failed")
        );
    }
}
