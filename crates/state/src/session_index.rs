use crate::token_usage::collect_codex_session_files;
use chrono::{DateTime, Utc};
use codex_companion_core::{CompanionError, Result, SessionPage, SessionSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

const SESSION_INDEX_VERSION: u32 = 2;
const RUNNING_WINDOW: Duration = Duration::from_secs(120);
const REQUEST_MARKERS: [&str; 2] = ["## My request for Codex:", "## My request:"];
const INJECTED_CONTEXT_TAGS: [&str; 2] = ["environment_context", "in-app-browser-context"];
static SESSION_INDEX_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SessionIndexCache {
    version: u32,
    files: BTreeMap<String, CachedSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSession {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    summary: SessionSummary,
}

pub fn list_sessions_cached(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
    query: Option<&str>,
    limit: usize,
    rebuild: bool,
) -> Result<SessionPage> {
    let lock = SESSION_INDEX_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| CompanionError::InvalidConfig("session index lock poisoned".into()))?;
    let cache_path = cache_dir.join("session-index.json");
    let mut cache = if rebuild {
        SessionIndexCache::default()
    } else {
        read_cache(&cache_path)
    };
    let files = collect_codex_session_files(&codex_dir);
    let mut summaries = Vec::new();
    let mut next_files = BTreeMap::new();
    let mut from_cache = !rebuild;

    for path in files {
        let Some(signature) = file_signature(&path) else {
            continue;
        };
        let key = path.to_string_lossy().to_string();
        let cached = cache
            .files
            .get(&key)
            .filter(|cached| cached.matches(&signature))
            .cloned();
        let session = match cached {
            Some(mut cached) => {
                cached.summary.is_running = signature.is_running;
                cached.summary.cwd_available =
                    cached.summary.cwd.as_deref().is_some_and(Path::is_dir);
                cached
            }
            None => {
                from_cache = false;
                CachedSession {
                    len: signature.len,
                    modified_secs: signature.modified_secs,
                    modified_nanos: signature.modified_nanos,
                    summary: parse_session_summary(&path, &signature)?,
                }
            }
        };
        summaries.push(session.summary.clone());
        next_files.insert(key, session);
    }

    cache.version = SESSION_INDEX_VERSION;
    cache.files = next_files;
    write_cache(&cache_path, &cache)?;
    summaries.sort_by_key(|summary| Reverse(summary.modified_at));
    let normalized_query = query.unwrap_or_default().trim().to_lowercase();
    if !normalized_query.is_empty() {
        summaries.retain(|session| session_matches(session, &normalized_query));
    }
    let total = summaries.len();
    summaries.truncate(limit.clamp(1, 200));

    Ok(SessionPage {
        sessions: summaries,
        total,
        query: query.unwrap_or_default().trim().to_string(),
        from_cache,
        data_root: codex_dir,
    })
}

impl CachedSession {
    fn matches(&self, signature: &FileSignature) -> bool {
        self.len == signature.len
            && self.modified_secs == signature.modified_secs
            && self.modified_nanos == signature.modified_nanos
    }
}

#[derive(Debug, Clone)]
struct FileSignature {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    modified_at: DateTime<Utc>,
    is_running: bool,
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let unix = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(FileSignature {
        len: metadata.len(),
        modified_secs: unix.as_secs(),
        modified_nanos: unix.subsec_nanos(),
        modified_at: DateTime::<Utc>::from(modified),
        is_running: path.components().any(|part| part.as_os_str() == "sessions")
            && modified
                .elapsed()
                .is_ok_and(|elapsed| elapsed <= RUNNING_WINDOW),
    })
}

fn parse_session_summary(path: &Path, signature: &FileSignature) -> Result<SessionSummary> {
    let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    let mut id =
        session_id_from_filename(path).unwrap_or_else(|| path.to_string_lossy().to_string());
    let mut title = String::new();
    let mut cwd = None;
    let mut model = "unknown".to_string();
    let mut provider_id = None;
    let mut parent_id = None;
    let mut is_subagent = false;

    for line in BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
    {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                id = first_string(payload, &["id", "thread_id", "threadId"]).unwrap_or(id);
                cwd = first_string(payload, &["cwd", "working_directory", "workingDirectory"])
                    .or(cwd);
                provider_id = first_string(
                    payload,
                    &[
                        "model_provider",
                        "modelProvider",
                        "provider_id",
                        "providerId",
                    ],
                )
                .or(provider_id);
                parent_id = payload
                    .get("forked_from_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        payload
                            .pointer("/source/subagent/thread_spawn/parent_thread_id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .or(parent_id);
                is_subagent = is_subagent
                    || payload.pointer("/source/subagent").is_some()
                    || parent_id.is_some();
            }
            Some("turn_context") => {
                if let Some(payload) = value.get("payload") {
                    model = first_string(payload, &["model", "model_name", "modelName"])
                        .unwrap_or(model);
                    provider_id = first_string(
                        payload,
                        &[
                            "model_provider",
                            "modelProvider",
                            "provider_id",
                            "providerId",
                        ],
                    )
                    .or(provider_id);
                }
            }
            _ => {}
        }
        if title.is_empty() {
            title = meaningful_user_message_text(&value).unwrap_or_default();
        }
    }

    if title.is_empty() {
        title = cwd
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| id.clone());
    }
    title = compact_title(&title);

    let cwd = cwd.map(PathBuf::from);
    let cwd_available = cwd.as_deref().is_some_and(Path::is_dir);
    Ok(SessionSummary {
        id,
        title,
        cwd,
        cwd_available,
        model,
        provider_id,
        path: path.to_path_buf(),
        modified_at: signature.modified_at,
        bytes: signature.len,
        is_subagent,
        parent_id,
        is_running: signature.is_running,
    })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn user_message_text(value: &Value) -> Option<String> {
    let payload = value.get("payload").unwrap_or(value);
    let role = payload.get("role").and_then(Value::as_str);
    let kind = payload.get("type").and_then(Value::as_str);
    if role != Some("user") && kind != Some("user_message") {
        return None;
    }
    first_string(payload, &["text", "message", "content"]).or_else(|| {
        payload
            .get("content")?
            .as_array()?
            .iter()
            .find_map(|part| first_string(part, &["text", "input_text", "inputText"]))
    })
}

fn meaningful_user_message_text(value: &Value) -> Option<String> {
    let message = user_message_text(value)?;
    let explicit_request = REQUEST_MARKERS
        .iter()
        .find_map(|marker| message.rsplit_once(marker).map(|(_, request)| request));
    let candidate = strip_leading_context_blocks(explicit_request.unwrap_or(&message));
    if candidate.is_empty() || candidate.starts_with("# AGENTS.md instructions for ") {
        return None;
    }
    let candidate = candidate
        .find("<image ")
        .map(|index| &candidate[..index])
        .unwrap_or(candidate);
    let title = compact_title(candidate);
    (!title.is_empty()).then_some(title)
}

fn strip_leading_context_blocks(mut value: &str) -> &str {
    loop {
        let trimmed = value.trim_start();
        let Some(tag) = INJECTED_CONTEXT_TAGS
            .iter()
            .find(|tag| starts_with_tag(trimmed, tag))
        else {
            return trimmed;
        };
        let closing_tag = format!("</{tag}>");
        let Some(end) = trimmed.find(&closing_tag) else {
            return "";
        };
        value = &trimmed[end + closing_tag.len()..];
    }
}

fn starts_with_tag(value: &str, tag: &str) -> bool {
    let Some(remainder) = value.strip_prefix(&format!("<{tag}")) else {
        return false;
    };
    remainder.starts_with('>') || remainder.starts_with(char::is_whitespace)
}

fn compact_title(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(120).collect()
}

fn session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let suffix = stem.rsplit('-').next()?;
    (!suffix.is_empty()).then(|| suffix.to_string())
}

fn session_matches(session: &SessionSummary, query: &str) -> bool {
    [
        session.id.as_str(),
        session.title.as_str(),
        session
            .cwd
            .as_deref()
            .and_then(Path::to_str)
            .unwrap_or_default(),
        session.model.as_str(),
        session.provider_id.as_deref().unwrap_or_default(),
        session.path.to_str().unwrap_or_default(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
}

fn read_cache(path: &Path) -> SessionIndexCache {
    let Ok(text) = fs::read_to_string(path) else {
        return SessionIndexCache {
            version: SESSION_INDEX_VERSION,
            ..SessionIndexCache::default()
        };
    };
    let Ok(cache) = serde_json::from_str::<SessionIndexCache>(&text) else {
        return SessionIndexCache::default();
    };
    if cache.version == SESSION_INDEX_VERSION {
        cache
    } else {
        SessionIndexCache::default()
    }
}

fn write_cache(path: &Path, cache: &SessionIndexCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }
    let text = serde_json::to_string(cache).map_err(|error| {
        CompanionError::InvalidConfig(format!("session index serialize failed: {error}"))
    })?;
    fs::write(path, text).map_err(|source| CompanionError::io(path, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_and_searches_session_first_page() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp.path().join("codex/sessions/2026/07/22");
        let project = temp.path().join("alpha");
        fs::create_dir_all(&day).expect("mkdir");
        fs::create_dir_all(&project).expect("project mkdir");
        let contents = [
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "session-a",
                    "cwd": project,
                    "model_provider": "openai",
                },
            }),
            serde_json::json!({
                "type": "turn_context",
                "payload": { "model": "gpt-5.6-codex" },
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "Repair the release workflow",
                    }],
                },
            }),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(day.join("rollout-session-a.jsonl"), contents).expect("write");
        let cache_dir = temp.path().join("cache");

        let first = list_sessions_cached(
            temp.path().join("codex"),
            cache_dir.clone(),
            None,
            50,
            false,
        )
        .expect("first");
        assert!(!first.from_cache);
        assert_eq!(first.sessions[0].title, "Repair the release workflow");
        assert_eq!(first.sessions[0].cwd.as_deref(), Some(project.as_path()));
        assert!(first.sessions[0].cwd_available);
        fs::remove_dir(&project).expect("remove project");
        let second = list_sessions_cached(
            temp.path().join("codex"),
            cache_dir,
            Some("release"),
            50,
            false,
        )
        .expect("second");
        assert!(second.from_cache);
        assert_eq!(second.total, 1);
        assert_eq!(second.sessions[0].id, "session-a");
        assert!(!second.sessions[0].cwd_available);
    }

    #[test]
    fn skips_injected_context_and_extracts_the_explicit_user_request() {
        let injected = serde_json::json!({
            "type": "response_item",
            "payload": {
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<environment_context><cwd>/tmp/project</cwd></environment_context>"
                }]
            }
        });
        let request = serde_json::json!({
            "type": "response_item",
            "payload": {
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "# Files mentioned by the user:\n\n## My request for Codex:\n让历史会话显示真正的用户请求\n<image name=\"screenshot\">"
                }]
            }
        });

        assert_eq!(meaningful_user_message_text(&injected), None);
        assert_eq!(
            meaningful_user_message_text(&request).as_deref(),
            Some("让历史会话显示真正的用户请求")
        );
    }

    #[test]
    fn skips_repository_instructions_when_choosing_a_title() {
        let instructions = serde_json::json!({
            "type": "response_item",
            "payload": {
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "# AGENTS.md instructions for /tmp/project\n\n<INSTRUCTIONS>Internal rules</INSTRUCTIONS>"
                }]
            }
        });

        assert_eq!(meaningful_user_message_text(&instructions), None);
    }
}
