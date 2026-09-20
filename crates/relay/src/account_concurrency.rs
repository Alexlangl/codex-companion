use codex_companion_core::ProviderConfig;
use codex_companion_provider::account_identity_key as account_concurrency_key;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

#[derive(Debug)]
pub(crate) struct AccountConcurrency {
    active: Mutex<HashMap<String, usize>>,
    changed: Notify,
    queue: Semaphore,
    directory: Option<PathBuf>,
}

impl Default for AccountConcurrency {
    fn default() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            queue: Semaphore::new(100),
            directory: None,
        }
    }
}

pub(crate) struct AccountPermit {
    key: String,
    owner: Arc<AccountConcurrency>,
    _file: Option<File>,
}

impl Drop for AccountPermit {
    fn drop(&mut self) {
        let mut active = self.owner.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = active.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.key);
            }
        }
        drop(active);
        self._file.take();
        self.owner.changed.notify_waiters();
    }
}

impl AccountConcurrency {
    pub(crate) fn shared(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
            ..Self::default()
        }
    }

    fn reserve(
        self: &Arc<Self>,
        provider: &ProviderConfig,
        limit: u16,
    ) -> Result<Option<Arc<AccountPermit>>, &'static str> {
        let key = account_concurrency_key(provider);
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        let count = active.get(&key).copied().unwrap_or(0);
        if limit > 0 && count >= usize::from(limit) {
            return Ok(None);
        }
        let mut file = None;
        if let Some(root) = self.directory.as_ref().filter(|_| limit > 0) {
            let dir = root.join(format!("{:x}", Sha256::digest(key.as_bytes())));
            std::fs::create_dir_all(&dir).map_err(|_| "account_concurrency_storage_unavailable")?;
            for slot in 0..limit {
                let candidate = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(dir.join(format!("{slot}.lock")))
                    .map_err(|_| "account_concurrency_storage_unavailable")?;
                match candidate.try_lock() {
                    Ok(()) => {
                        file = Some(candidate);
                        break;
                    }
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(_) => return Err("account_concurrency_storage_unavailable"),
                }
            }
            if file.is_none() {
                return Ok(None);
            }
        }
        active.insert(key.clone(), count + 1);
        Ok(Some(Arc::new(AccountPermit {
            key,
            owner: self.clone(),
            _file: file,
        })))
    }

    pub(crate) async fn acquire_any(
        self: &Arc<Self>,
        providers: &[ProviderConfig],
        limit: u16,
        wait_ms: u64,
        eligible: impl Fn(&ProviderConfig) -> bool,
    ) -> Result<(usize, Arc<AccountPermit>), &'static str> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(120_000));
        let mut queued = None;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut available = false;
            for (index, provider) in providers.iter().enumerate().filter(|(_, p)| eligible(p)) {
                available = true;
                if let Some(permit) = self.reserve(provider, limit)? {
                    return Ok((index, permit));
                }
            }
            if !available {
                return Err("account_policy_unavailable");
            }
            if wait_ms == 0 {
                return Err("account_concurrency_exceeded");
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("account_concurrency_wait_timeout");
            }
            if queued.is_none() {
                queued = Some(
                    self.queue
                        .try_acquire()
                        .map_err(|_| "account_concurrency_queue_full")?,
                );
            }
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(500))) => {},
            }
        }
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        provider: &ProviderConfig,
        limit: u16,
        wait_ms: u64,
    ) -> Result<Arc<AccountPermit>, &'static str> {
        self.acquire_any(std::slice::from_ref(provider), limit, wait_ms, |_| true)
            .await
            .map(|(_, permit)| permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider(id: &str) -> ProviderConfig {
        serde_json::from_value(serde_json::json!({"id":id,"name":id,"kind":"official_codex","baseUrl":"https://example.test","authRef":"file:fixture-auth","modelMap":{},"priority":0,"enabled":true})).unwrap()
    }

    #[tokio::test]
    async fn slots_are_shared_across_processes() {
        const FLAG: &str = "COMPANION_SLOT_TEST_DIRECTORY";
        if let Some(directory) = std::env::var_os(FLAG) {
            let gate = Arc::new(AccountConcurrency::shared(directory.into()));
            assert_eq!(
                gate.acquire(&provider("alias"), 1, 0).await.err(),
                Some("account_concurrency_exceeded")
            );
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let gate = Arc::new(AccountConcurrency::shared(temp.path().to_path_buf()));
        let held = gate.acquire(&provider("owner"), 1, 0).await.unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "account_concurrency::tests::slots_are_shared_across_processes",
                "--nocapture",
            ])
            .env(FLAG, temp.path())
            .status()
            .unwrap();
        assert!(status.success());
        drop(held);
        let other = Arc::new(AccountConcurrency::shared(temp.path().to_path_buf()));
        assert!(other.acquire(&provider("alias"), 1, 0).await.is_ok());
    }

    #[tokio::test]
    async fn waits_for_any_candidate_and_rechecks_eligibility() {
        let gate = Arc::new(AccountConcurrency::default());
        let a = provider("a");
        let mut b = provider("b");
        b.auth_ref = Some("file:other".into());
        let held_a = gate.acquire(&a, 1, 0).await.unwrap();
        let held_b = gate.acquire(&b, 1, 0).await.unwrap();
        let owner = gate.clone();
        let task =
            tokio::spawn(async move { owner.acquire_any(&[a, b], 1, 2000, |p| p.id == "a").await });
        tokio::task::yield_now().await;
        drop(held_b);
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        drop(held_a);
        assert_eq!(task.await.unwrap().unwrap().0, 0);
    }

    #[tokio::test]
    async fn duplicate_provider_cannot_bypass_limit_and_release_wakes_waiter() {
        let gate = Arc::new(AccountConcurrency::default());
        let first = gate.acquire(&provider("a"), 1, 0).await.unwrap();
        assert_eq!(
            gate.acquire(&provider("alias"), 1, 0).await.err(),
            Some("account_concurrency_exceeded")
        );
        assert_eq!(
            gate.acquire(&provider("alias"), 1, 5).await.err(),
            Some("account_concurrency_wait_timeout")
        );
        let owner = gate.clone();
        let waiting = tokio::spawn(async move { owner.acquire(&provider("alias"), 1, 1000).await });
        tokio::task::yield_now().await;
        drop(first);
        let second = waiting.await.unwrap().unwrap();
        drop(second);
        assert!(gate.active.lock().unwrap().is_empty());
        assert_eq!(gate.queue.available_permits(), 100);
    }

    #[tokio::test]
    async fn cancellation_releases_waiting_slot() {
        let gate = Arc::new(AccountConcurrency::default());
        let first = gate.acquire(&provider("a"), 1, 0).await.unwrap();
        let owner = gate.clone();
        let waiting = tokio::spawn(async move { owner.acquire(&provider("a"), 1, 1000).await });
        tokio::task::yield_now().await;
        waiting.abort();
        let _ = waiting.await;
        assert_eq!(gate.queue.available_permits(), 100);
        drop(first);
        assert!(gate.acquire(&provider("a"), 1, 0).await.is_ok());
    }
}
