use crate::{CompanionError, DiagnosticInfo, Result};
use chrono::Utc;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const RETAINED_LOG_FILES: usize = 5;
static LOG_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn append_diagnostic_log(
    data_dir: &Path,
    level: &str,
    source: &str,
    message: &str,
) -> Result<()> {
    let _guard = LOG_WRITE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| CompanionError::InvalidConfig("diagnostic log lock poisoned".to_string()))?;
    let log_dir = diagnostic_log_directory(data_dir);
    fs::create_dir_all(&log_dir).map_err(|source| CompanionError::io(&log_dir, source))?;
    let current_path = log_dir.join("companion.log.jsonl");
    rotate_if_needed(&current_path)?;
    let entry = serde_json::json!({
        "timestamp": Utc::now(),
        "level": level,
        "source": source,
        "message": redact_text(message),
    });
    let text = serde_json::to_string(&entry).map_err(|source| {
        CompanionError::InvalidConfig(format!("diagnostic log serialize failed: {source}"))
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current_path)
        .map_err(|source| CompanionError::io(&current_path, source))?;
    writeln!(file, "{text}").map_err(|source| CompanionError::io(&current_path, source))
}

pub fn diagnostic_info(data_dir: &Path) -> DiagnosticInfo {
    let log_directory = diagnostic_log_directory(data_dir);
    let current_log_path = log_directory.join("companion.log.jsonl");
    let files = fs::read_dir(&log_directory)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .collect::<Vec<_>>();
    let total_bytes = files
        .iter()
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum();
    DiagnosticInfo {
        log_directory,
        current_log_path,
        retained_files: files.len(),
        total_bytes,
    }
}

pub fn clear_diagnostic_logs(data_dir: &Path) -> Result<usize> {
    let log_directory = diagnostic_log_directory(data_dir);
    let Ok(entries) = fs::read_dir(&log_directory) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("companion.log"))
        {
            fs::remove_file(&path).map_err(|source| CompanionError::io(&path, source))?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn redact_diagnostic_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let sensitive = [
                        "token",
                        "authorization",
                        "cookie",
                        "secret",
                        "private_key",
                        "apikey",
                        "api_key",
                        "password",
                    ]
                    .iter()
                    .any(|marker| lower.contains(marker));
                    let value = if sensitive {
                        Value::String("[redacted]".to_string())
                    } else {
                        redact_diagnostic_value(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_diagnostic_value).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        _ => value.clone(),
    }
}

fn diagnostic_log_directory(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

fn rotate_if_needed(current_path: &Path) -> Result<()> {
    if fs::metadata(current_path)
        .map(|metadata| metadata.len())
        .unwrap_or_default()
        < MAX_LOG_BYTES
    {
        return Ok(());
    }
    for index in (1..RETAINED_LOG_FILES.saturating_sub(1)).rev() {
        let source = current_path.with_file_name(format!("companion.log.{index}.jsonl"));
        let target = current_path.with_file_name(format!("companion.log.{}.jsonl", index + 1));
        if source.exists() {
            let _ = fs::remove_file(&target);
            fs::rename(&source, &target).map_err(|source| CompanionError::io(&target, source))?;
        }
    }
    let first = current_path.with_file_name("companion.log.1.jsonl");
    let _ = fs::remove_file(&first);
    fs::rename(current_path, &first).map_err(|source| CompanionError::io(&first, source))
}

fn redact_text(text: &str) -> String {
    let mut output = text.to_string();
    for prefix in ["Bearer ", "AgentAssertion "] {
        output = redact_after_prefix(&output, prefix);
    }
    output
}

fn redact_after_prefix(text: &str, prefix: &str) -> String {
    let mut output = text.to_string();
    let mut offset = 0;
    while let Some(relative_start) = output[offset..].find(prefix) {
        let value_start = offset + relative_start + prefix.len();
        let end = output[value_start..]
            .find(|character: char| character.is_ascii_whitespace() || "\"',}".contains(character))
            .map(|end_offset| value_start + end_offset)
            .unwrap_or(output.len());
        output.replace_range(value_start..end, "[redacted]");
        offset = value_start + "[redacted]".len();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_structured_and_header_secrets() {
        let value = serde_json::json!({
            "access_token": "secret",
            "nested": { "message": "Authorization Bearer abc123" }
        });
        let redacted = redact_diagnostic_value(&value);
        assert_eq!(redacted["access_token"], "[redacted]");
        assert_eq!(
            redacted.pointer("/nested/message").and_then(Value::as_str),
            Some("Authorization Bearer [redacted]")
        );
    }

    #[test]
    fn rotation_retains_at_most_five_log_files_including_the_current_file() {
        let temp = tempfile::tempdir().expect("temp");
        let log_dir = diagnostic_log_directory(temp.path());
        fs::create_dir_all(&log_dir).expect("log dir");
        let current = log_dir.join("companion.log.jsonl");

        for index in 0..(RETAINED_LOG_FILES + 2) {
            let file = fs::File::create(&current).expect("current log");
            file.set_len(MAX_LOG_BYTES).expect("large log");
            append_diagnostic_log(temp.path(), "info", "test", &format!("entry {index}"))
                .expect("append");
        }

        let info = diagnostic_info(temp.path());
        assert_eq!(info.retained_files, RETAINED_LOG_FILES);
        assert!(current.exists());
        assert!(log_dir.join("companion.log.4.jsonl").exists());
        assert!(!log_dir.join("companion.log.5.jsonl").exists());
    }
}
