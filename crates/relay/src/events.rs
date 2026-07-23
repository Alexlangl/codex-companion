use codex_companion_core::{append_diagnostic_log, now_event, ConfigStore};
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

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
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    if let Ok(text) = serde_json::to_string(&event) {
        let _ = writeln!(file, "{text}");
    }
}
