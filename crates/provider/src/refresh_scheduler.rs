use codex_companion_core::{CompanionError, ConfigStore, ProviderHealth, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{LazyLock, Mutex},
    time::Duration,
};
use tokio::sync::{watch, Semaphore};

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRefresh {
    completed_at: i64,
    credential: Option<String>,
    health: ProviderHealth,
    account: Option<codex_companion_core::ProviderAccountInfo>,
}

type Outcome = std::result::Result<
    (
        ProviderHealth,
        Option<codex_companion_core::ProviderAccountInfo>,
    ),
    String,
>;
type Key = (PathBuf, String);
static RUNNING: LazyLock<Mutex<HashMap<Key, watch::Receiver<Option<Outcome>>>>> =
    LazyLock::new(Default::default);
static MANUAL: Semaphore = Semaphore::const_new(1);
static BACKGROUND: Semaphore = Semaphore::const_new(1);

struct Registration(Key);
impl Drop for Registration {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Single flight per account, bounded admission, independent manual/background
/// lanes. Callers share the result instead of queueing duplicate quota probes.
pub(crate) async fn refresh(
    store: &ConfigStore,
    id: &str,
    background: bool,
) -> Result<ProviderHealth> {
    let config = store.load()?;
    let original = config
        .providers
        .get(id)
        .cloned()
        .ok_or_else(|| CompanionError::InvalidConfig("unknown provider".into()))?;
    let requested_at = chrono::Utc::now().timestamp_micros();
    let original_token = crate::auth::resolve_auth_token(&original);
    let credential = original_token
        .as_deref()
        .map(|token| format!("{:x}", Sha256::digest(token.as_bytes())));
    let official = original.kind == codex_companion_core::ProviderKind::OfficialCodex;
    let identity = crate::auth::account_identity_key(&original);
    let key = (
        store.path().to_path_buf(),
        if official { identity } else { id.to_string() },
    );
    let mut receiver = {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(receiver) = running.get(&key) {
            receiver.clone()
        } else {
            if running.len() >= 16 {
                return Err(CompanionError::InvalidConfig(
                    "额度刷新队列已满，请稍后重试".into(),
                ));
            }
            let (sender, receiver) = watch::channel(None);
            running.insert(key.clone(), receiver.clone());
            let store = store.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let cache = store
                    .data_dir()
                    .join("quota-coordination")
                    .join(format!("{:x}.json", Sha256::digest(key.1.as_bytes())));
                let _registration = Registration(key);
                let lane = if background { &BACKGROUND } else { &MANUAL };
                let result = tokio::time::timeout(Duration::from_secs(120), async {
                    let _permit = lane
                        .acquire()
                        .await
                        .map_err(|_| "额度刷新通道已关闭".to_string())?;
                    std::fs::create_dir_all(cache.parent().unwrap()).map_err(|e| e.to_string())?;
                    let _file_guard = super::codex_oauth::lock_auth_file_with_timeout(
                        &cache,
                        Duration::from_secs(120),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    if let Some(previous) = std::fs::read(&cache)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<CachedRefresh>(&bytes).ok())
                    {
                        if previous.completed_at >= requested_at
                            && previous.credential == credential
                        {
                            return Ok((previous.health, previous.account));
                        }
                    }
                    let health = super::refresh::refresh_provider_status_inner(&store, &id)
                        .await
                        .map_err(|e| e.to_string())?;
                    let account = store
                        .load()
                        .map_err(|e| e.to_string())?
                        .providers
                        .get(&id)
                        .and_then(|p| p.account.clone());
                    let previous = CachedRefresh {
                        completed_at: chrono::Utc::now().timestamp_micros(),
                        credential,
                        health: health.clone(),
                        account: account.clone(),
                    };
                    if let Ok(bytes) = serde_json::to_vec(&previous) {
                        let _ = codex_companion_core::atomic_write_private_file(&cache, &bytes);
                    }
                    Ok((health, account))
                })
                .await
                .unwrap_or_else(|_| Err("额度刷新任务超时".to_string()));
                sender.send_replace(Some(result));
            });
            receiver
        }
    };
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            let (health, account) = result.map_err(CompanionError::InvalidConfig)?;
            // Share account quota data without copying another provider's relay
            // failures, name, endpoints, routing or credentials.
            if official {
                store.update(|config| {
                    if let Some(current) = config.providers.get_mut(id).filter(|p| {
                        **p == original && crate::auth::resolve_auth_token(p) == original_token
                    }) {
                        if let (Some(current), Some(ref refreshed)) =
                            (current.account.as_mut(), account.as_ref())
                        {
                            current.quota_windows = refreshed.quota_windows.clone();
                            current.quota_percent = refreshed.quota_percent;
                            current.quota_reset_at = refreshed.quota_reset_at.clone();
                            current.quota_label = refreshed.quota_label.clone();
                            current.quota_hourly_present = refreshed.quota_hourly_present;
                            current.quota_weekly_present = refreshed.quota_weekly_present;
                            current.last_refresh_at = refreshed.last_refresh_at.clone();
                        }
                        let target = config.health.entry(id.to_string()).or_default();
                        target.refresh_failure_count = health.refresh_failure_count;
                        target.next_refresh_after = health.next_refresh_after;
                        target.refresh_error = health.refresh_error.clone();
                        target.last_refresh_attempt = health.last_refresh_attempt;
                    }
                    Ok(())
                })?;
            }
            return Ok(store.load()?.health.get(id).cloned().unwrap_or(health));
        }
        receiver
            .changed()
            .await
            .map_err(|_| CompanionError::InvalidConfig("额度刷新任务已中止".into()))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn aliases_share_quota_but_keep_independent_relay_health() {
        use codex_companion_core::{HealthStatusKind, ProviderAccountInfo, ProviderConfig};
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.json"));
        let auth_path = dir.path().join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"tokens":{"access_token":"fixture","user_id":"u","account_id":"a"}}"#,
        )
        .unwrap();
        let make = |id| {
            serde_json::from_value::<ProviderConfig>(serde_json::json!({"id":id,"name":id,"kind":"official_codex","baseUrl":"https://example.test","authRef":format!("file:{}",auth_path.display()),"modelMap":{},"priority":0,"enabled":true,"account":{"accountId":"a","userId":"u"}})).unwrap()
        };
        let a = make("a");
        let b = make("b");
        store
            .update(|config| {
                config.providers.insert("a".into(), a.clone());
                config.providers.insert("b".into(), b);
                config.health.insert(
                    "b".into(),
                    ProviderHealth {
                        status: HealthStatusKind::AuthFailed,
                        ..Default::default()
                    },
                );
                Ok(())
            })
            .unwrap();
        let identity = crate::auth::account_identity_key(&a);
        let cached = CachedRefresh {
            completed_at: chrono::Utc::now().timestamp_micros() + 1_000_000,
            credential: Some(format!("{:x}", Sha256::digest(b"fixture"))),
            health: ProviderHealth::default(),
            account: Some(ProviderAccountInfo {
                quota_percent: Some(77.0),
                last_refresh_at: Some(chrono::Utc::now().to_rfc3339()),
                ..Default::default()
            }),
        };
        let path = store
            .data_dir()
            .join("quota-coordination")
            .join(format!("{:x}.json", Sha256::digest(identity.as_bytes())));
        codex_companion_core::atomic_write_private_file(
            &path,
            &serde_json::to_vec(&cached).unwrap(),
        )
        .unwrap();
        let (first, second) = tokio::join!(refresh(&store, "a", false), refresh(&store, "b", true));
        first.unwrap();
        assert_eq!(second.unwrap().status, HealthStatusKind::AuthFailed);
        let config = store.load().unwrap();
        assert_eq!(
            config.providers["a"]
                .account
                .as_ref()
                .unwrap()
                .quota_percent,
            Some(77.0)
        );
        assert_eq!(
            config.providers["b"]
                .account
                .as_ref()
                .unwrap()
                .quota_percent,
            Some(77.0)
        );
        assert_eq!(config.providers["b"].name, "b");
    }

    #[tokio::test]
    async fn concurrent_manual_and_background_refresh_share_one_probe() {
        let count = Arc::new(AtomicUsize::new(0));
        let hits = count.clone();
        let app = Router::new().route(
            "/v1/models",
            get(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    "{}"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.json"));
        let provider = serde_json::from_value(serde_json::json!({"id":"test","name":"test","kind":"openai_compatible","baseUrl":url,"authRef":null,"modelMap":{},"priority":0,"enabled":true})).unwrap();
        store
            .update(|config| {
                config.providers.insert("test".into(), provider);
                Ok(())
            })
            .unwrap();
        let results =
            futures_util::future::join_all((0..8).map(|i| refresh(&store, "test", i % 2 == 0)))
                .await;
        for result in results {
            result.unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
