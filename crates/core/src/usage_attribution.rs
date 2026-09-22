//! Minimal billing provenance, independent of disposable request/diagnostic logs.
//! No prompts, response text, headers or credentials are retained.
use crate::{CompanionError, Result, TokenUsageEvent};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

fn db_error(error: rusqlite::Error) -> CompanionError {
    CompanionError::InvalidConfig(format!("用量来源数据库: {error}"))
}

/// Record only an actual terminal Responses result, after protocol conversion.
pub fn record_usage_attribution(
    data_dir: &Path,
    request_id: &str,
    session_id: &str,
    provider_id: &str,
    value: &Value,
) -> Result<()> {
    let kind = value.get("type").and_then(Value::as_str);
    if kind.is_some() && !matches!(kind, Some("response.completed" | "response.incomplete")) {
        return Ok(());
    }
    let response = value.get("response").unwrap_or(value);
    if !matches!(
        response.get("status").and_then(Value::as_str),
        Some("completed" | "incomplete")
    ) {
        return Ok(());
    }
    let Some(usage) = response.get("usage") else {
        return Ok(());
    };
    let (Some(input), Some(output)) = (
        usage.get("input_tokens").and_then(Value::as_u64),
        usage.get("output_tokens").and_then(Value::as_u64),
    ) else {
        return Ok(());
    };
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(input);
    if session_id.is_empty()
        || session_id.len() > 256
        || provider_id.is_empty()
        || input > i64::MAX as u64
        || output > i64::MAX as u64
    {
        return Ok(());
    }
    std::fs::create_dir_all(data_dir).map_err(|source| CompanionError::io(data_dir, source))?;
    let db = Connection::open(data_dir.join("usage-attribution.sqlite3")).map_err(db_error)?;
    db.busy_timeout(Duration::from_secs(2)).map_err(db_error)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS usage_attribution (
        request_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, provider_id TEXT NOT NULL,
        input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
        completed_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS usage_attribution_session ON usage_attribution(session_id, completed_at);").map_err(db_error)?;
    db.execute(
        "INSERT OR IGNORE INTO usage_attribution VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            request_id,
            session_id,
            provider_id,
            input as i64,
            cached as i64,
            output as i64,
            Utc::now().timestamp_millis()
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

/// Match an event's session, full input/cached/output counters and completion time.
/// Ambiguous matches and pre-upgrade history remain unassigned; never use current routing.
pub fn apply_usage_attribution(data_dir: &Path, events: &mut [TokenUsageEvent]) -> Result<()> {
    let path = data_dir.join("usage-attribution.sqlite3");
    if !path.exists() {
        return Ok(());
    }
    let db =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(db_error)?;
    db.busy_timeout(Duration::from_secs(2)).map_err(db_error)?;
    let mut grouped = BTreeMap::<String, Vec<usize>>::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(session) = &event.session_id {
            grouped.entry(session.clone()).or_default().push(index);
        }
    }
    let mut stmt = db.prepare("SELECT provider_id, input_tokens, cached_tokens, output_tokens, completed_at FROM usage_attribution WHERE session_id = ?1").map_err(db_error)?;
    for (session, indices) in grouped {
        let rows = stmt
            .query_map([session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(db_error)?;
        let mut matches = BTreeMap::<(u64, u64, u64), Vec<(i64, String)>>::new();
        for row in rows {
            let (provider, input, cached, output, time) = row.map_err(db_error)?;
            matches
                .entry((input, cached, output))
                .or_default()
                .push((time, provider));
        }
        for index in indices {
            let event = &mut events[index];
            let Some(time) = event
                .timestamp
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.timestamp_millis())
            else {
                continue;
            };
            let input = event
                .input_tokens
                .saturating_add(event.cached_input_tokens)
                .saturating_add(event.cache_write_input_tokens);
            let Some(candidates) =
                matches.get(&(input, event.cached_input_tokens, event.output_tokens))
            else {
                continue;
            };
            let providers: BTreeSet<_> = candidates
                .iter()
                .filter(|(completed, _)| {
                    (-2_000..=30_000).contains(&time.saturating_sub(*completed))
                })
                .map(|(_, p)| p)
                .collect();
            if providers.len() == 1 {
                event.provider_id = providers.first().map(|p| (*p).clone());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn matches_actual_provider_with_exact_counters_and_rejects_ambiguous_or_old_usage() {
        let dir = tempfile::tempdir().unwrap();
        let result = json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":80}}}});
        record_usage_attribution(dir.path(), "req-a", "s1", "provider-a", &result).unwrap();
        // Idempotent delivery cannot overwrite the original provider.
        record_usage_attribution(dir.path(), "req-a", "s1", "provider-b", &result).unwrap();
        let event = TokenUsageEvent {
            session_id: Some("s1".into()),
            timestamp: Some(Utc::now().to_rfc3339()),
            provider_id: Some("codex-companion".into()),
            input_tokens: 20,
            cached_input_tokens: 80,
            output_tokens: 20,
            ..Default::default()
        };
        let mut events = vec![
            event.clone(),
            TokenUsageEvent {
                session_id: Some("other".into()),
                ..event.clone()
            },
            TokenUsageEvent {
                timestamp: Some("2020-01-01T00:00:00Z".into()),
                ..event.clone()
            },
            TokenUsageEvent {
                output_tokens: 21,
                ..event.clone()
            },
        ];
        apply_usage_attribution(dir.path(), &mut events).unwrap();
        assert_eq!(events[0].provider_id.as_deref(), Some("provider-a"));
        assert!(events[1..]
            .iter()
            .all(|e| e.provider_id.as_deref() == Some("codex-companion")));
        record_usage_attribution(dir.path(), "req-b", "s1", "provider-b", &result).unwrap();
        let mut events = vec![event];
        apply_usage_attribution(dir.path(), &mut events).unwrap();
        assert_eq!(events[0].provider_id.as_deref(), Some("codex-companion"));
    }
    #[test]
    fn failed_or_missing_usage_creates_no_provenance() {
        let dir = tempfile::tempdir().unwrap();
        for value in [
            json!({"type":"response.failed"}),
            json!({"status":"completed"}),
        ] {
            record_usage_attribution(dir.path(), "r", "s", "p", &value).unwrap();
        }
        assert!(!dir.path().join("usage-attribution.sqlite3").exists());
    }
}
