use codex_companion_core::{
    append_diagnostic_log, now_event, ConfigStore, HealthStatusKind, RelayEvent,
};
use codex_companion_health::mark_success;
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const RELAY_EVENT_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const RELAY_EVENT_BACKUP_BYTES: u64 = 512 * 1024;
const RELAY_EVENT_TAIL_READ_BYTES: u64 = 512 * 1024;
const HEALTH_SUCCESS_PERSIST_INTERVAL: Duration = Duration::from_secs(15);
static RELAY_EVENT_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static HEALTH_SUCCESS_CHECKPOINTS: OnceLock<
    Mutex<HashMap<(PathBuf, String), HealthSuccessCheckpoint>>,
> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HealthSuccessCheckpoint {
    at: Instant,
    writing: bool,
}

pub(crate) fn record_health_success(store: &ConfigStore, provider_id: &str) -> bool {
    let key = (store.path().to_path_buf(), provider_id.to_string());
    let now = Instant::now();
    let observed = {
        let mut checkpoints = HEALTH_SUCCESS_CHECKPOINTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match checkpoints.get(&key).copied() {
            Some(checkpoint) if checkpoint.writing => return false,
            Some(checkpoint)
                if now.saturating_duration_since(checkpoint.at)
                    < HEALTH_SUCCESS_PERSIST_INTERVAL =>
            {
                Some(checkpoint)
            }
            _ => {
                checkpoints.insert(
                    key.clone(),
                    HealthSuccessCheckpoint {
                        at: now,
                        writing: true,
                    },
                );
                None
            }
        }
    };

    if let Some(observed) = observed {
        let already_healthy = store
            .load()
            .ok()
            .and_then(|config| {
                config
                    .health
                    .get(provider_id)
                    .map(|health| health.status.clone())
            })
            .is_some_and(|status| status == HealthStatusKind::Healthy);
        if already_healthy {
            return false;
        }
        let mut checkpoints = HEALTH_SUCCESS_CHECKPOINTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if checkpoints.get(&key) != Some(&observed) {
            return false;
        }
        checkpoints.insert(
            key.clone(),
            HealthSuccessCheckpoint {
                at: now,
                writing: true,
            },
        );
    }

    let persisted = store
        .update(|config| {
            mark_success(config.health.entry(provider_id.to_string()).or_default());
            Ok(())
        })
        .is_ok();
    let mut checkpoints = HEALTH_SUCCESS_CHECKPOINTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let claimed = HealthSuccessCheckpoint {
        at: now,
        writing: true,
    };
    if persisted {
        if checkpoints.get(&key) == Some(&claimed) {
            checkpoints.insert(
                key,
                HealthSuccessCheckpoint {
                    at: now,
                    writing: false,
                },
            );
        }
        return true;
    }
    if checkpoints.get(&key) == Some(&claimed) {
        checkpoints.remove(&key);
    }
    false
}

pub(crate) fn update_health<F>(store: &ConfigStore, provider_id: &str, update: F)
where
    F: FnOnce(&mut codex_companion_core::ProviderHealth),
{
    HEALTH_SUCCESS_CHECKPOINTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(store.path().to_path_buf(), provider_id.to_string()));
    let _ = store.update(|config| {
        let health = config.health.entry(provider_id.to_string()).or_default();
        update(health);
        Ok(())
    });
}

pub(crate) fn append_event(
    store: &ConfigStore,
    kind: &str,
    provider_id: Option<String>,
    message: String,
) {
    let diagnostic_level = if kind == "error" { "error" } else { "info" };
    let _ = append_diagnostic_log(&store.data_dir(), diagnostic_level, "relay", &message);
    let event = now_event(kind, provider_id, message);
    let events_dir = store.data_dir().join("relay");
    if fs::create_dir_all(&events_dir).is_err() {
        return;
    }
    let path = events_dir.join("events.jsonl");
    if let Ok(_guard) = RELAY_EVENT_LOG_LOCK.get_or_init(|| Mutex::new(())).lock() {
        let _ = append_event_to_path(
            &path,
            &event,
            RELAY_EVENT_LOG_MAX_BYTES,
            RELAY_EVENT_BACKUP_BYTES,
        );
    }
}

pub fn read_recent_events(data_dir: &Path, limit: usize) -> Vec<RelayEvent> {
    if limit == 0 {
        return Vec::new();
    }
    let _guard = RELAY_EVENT_LOG_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .ok();
    let events_dir = data_dir.join("relay");
    let paths = [
        events_dir.join("events.previous.jsonl"),
        events_dir.join("events.jsonl"),
    ];
    let mut events = Vec::new();
    for path in paths {
        let Ok(bytes) = read_tail_bytes(&path, RELAY_EVENT_TAIL_READ_BYTES) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<RelayEvent>(line).ok()),
        );
    }
    if events.len() > limit {
        events = events.split_off(events.len() - limit);
    }
    events
}

fn append_event_to_path(
    path: &Path,
    event: &RelayEvent,
    max_bytes: u64,
    backup_bytes: u64,
) -> std::io::Result<()> {
    let text = serde_json::to_string(event).map_err(std::io::Error::other)?;
    let incoming_bytes = u64::try_from(text.len().saturating_add(1)).unwrap_or(u64::MAX);
    let current_bytes = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if current_bytes.saturating_add(incoming_bytes) > max_bytes {
        let backup_path = path.with_file_name("events.previous.jsonl");
        let retained = read_tail_bytes(path, backup_bytes).unwrap_or_default();
        fs::write(backup_path, retained)?;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{text}")
}

fn read_tail_bytes(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let start = file_len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(file_len.saturating_sub(start)).unwrap_or(usize::MAX));
    file.read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=first_newline);
        } else {
            bytes.clear();
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_log_rotation_keeps_recent_events_and_bounds_file_sizes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let events_dir = temp.path().join("relay");
        fs::create_dir_all(&events_dir).expect("events dir");
        let path = events_dir.join("events.jsonl");
        for index in 0..30 {
            let event = now_event("request", None, format!("event-{index}-{}", "x".repeat(40)));
            append_event_to_path(&path, &event, 512, 512).expect("append event");
        }

        let events = read_recent_events(temp.path(), 5);
        assert_eq!(events.len(), 5);
        assert!(events
            .last()
            .is_some_and(|event| event.message.contains("event-29")));
        assert!(path.metadata().expect("current metadata").len() <= 512);
        assert!(
            events_dir
                .join("events.previous.jsonl")
                .metadata()
                .expect("backup metadata")
                .len()
                <= 512
        );
    }

    #[test]
    fn successful_health_writes_are_coalesced_until_a_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = ConfigStore::new(temp.path().join("config.json"));

        assert!(record_health_success(&store, "provider-a"));
        let first_checked = store
            .load()
            .expect("first health")
            .health
            .get("provider-a")
            .and_then(|health| health.last_checked)
            .expect("first timestamp");
        assert!(!record_health_success(&store, "provider-a"));
        assert_eq!(
            store
                .load()
                .expect("coalesced health")
                .health
                .get("provider-a")
                .and_then(|health| health.last_checked),
            Some(first_checked)
        );

        update_health(&store, "provider-a", |health| {
            health.status = HealthStatusKind::AuthFailed;
        });
        assert!(record_health_success(&store, "provider-a"));
        assert_eq!(
            store
                .load()
                .expect("recovered health")
                .health
                .get("provider-a")
                .map(|health| &health.status),
            Some(&HealthStatusKind::Healthy)
        );
    }

    #[test]
    fn concurrent_successes_share_one_persistence_checkpoint() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(17));
        let mut workers = Vec::new();
        for _ in 0..16 {
            let store = store.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                record_health_success(&store, "provider-a")
            }));
        }
        barrier.wait();

        let persisted = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().expect("success worker")))
            .sum::<usize>();
        assert_eq!(persisted, 1);
    }

    #[test]
    fn success_persists_after_an_external_failure_write() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = ConfigStore::new(temp.path().join("config.json"));
        assert!(record_health_success(&store, "provider-a"));

        store
            .update(|config| {
                config
                    .health
                    .entry("provider-a".to_string())
                    .or_default()
                    .status = HealthStatusKind::AuthFailed;
                Ok(())
            })
            .expect("external failure");

        assert!(record_health_success(&store, "provider-a"));
        assert_eq!(
            store
                .load()
                .expect("recovered health")
                .health
                .get("provider-a")
                .map(|health| &health.status),
            Some(&HealthStatusKind::Healthy)
        );
    }
}
