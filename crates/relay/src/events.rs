use codex_companion_core::{append_diagnostic_log, now_event, ConfigStore, RelayEvent};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Mutex, OnceLock},
};

const RELAY_EVENT_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const RELAY_EVENT_BACKUP_BYTES: u64 = 512 * 1024;
const RELAY_EVENT_TAIL_READ_BYTES: u64 = 512 * 1024;
static RELAY_EVENT_LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn update_health<F>(store: &ConfigStore, provider_id: &str, update: F)
where
    F: FnOnce(&mut codex_companion_core::ProviderHealth),
{
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
}
