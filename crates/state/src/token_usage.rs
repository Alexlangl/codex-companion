use crate::pricing::{default_pricing_override_path, CostBreakdown, PricingCatalog, PRICING_AS_OF};
use chrono::{DateTime, Local};
use codex_companion_core::{
    CompanionError, Result, TokenUsageBucket, TokenUsageEvent, TokenUsageSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const TOKEN_USAGE_CACHE_VERSION: u32 = 2;

#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

#[derive(Debug, Clone, Default)]
struct DeltaTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.output == 0
    }

    fn fresh_input(&self) -> u64 {
        self.input.saturating_sub(self.cached_input)
    }

    fn total(&self) -> u64 {
        self.fresh_input() + self.cached_input + self.output
    }
}

#[derive(Debug, Clone)]
struct FileParseState {
    session_id: Option<String>,
    current_model: String,
    current_provider_id: Option<String>,
    prev_total: Option<CumulativeTokens>,
}

impl Default for FileParseState {
    fn default() -> Self {
        Self {
            session_id: None,
            current_model: "unknown".to_string(),
            current_provider_id: None,
            prev_total: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TokenUsageCache {
    version: u32,
    files: BTreeMap<String, CachedTokenUsageFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedTokenUsageFile {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
    events: Vec<TokenUsageEvent>,
}

pub fn collect_token_usage(codex_dir: PathBuf) -> Result<TokenUsageSummary> {
    let files = collect_codex_session_files(&codex_dir);
    let mut all_events = Vec::new();
    for file in &files {
        all_events.extend(parse_session_file(file)?);
    }
    Ok(summarize_token_events(
        codex_dir,
        files.len(),
        all_events,
        &PricingCatalog::builtin(),
    ))
}

pub fn collect_token_usage_cached(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
) -> Result<TokenUsageSummary> {
    let files = collect_codex_session_files(&codex_dir);
    let cache_path = cache_dir.join("token-usage-cache.json");
    let mut cache = read_token_usage_cache(&cache_path);
    let mut next_files = BTreeMap::new();
    let mut all_events = Vec::new();

    for file in &files {
        let cache_key = file.to_string_lossy().to_string();
        let signature = file_signature(file);
        let cached = signature.as_ref().and_then(|signature| {
            cache
                .files
                .get(&cache_key)
                .filter(|cached| cached.matches(signature))
                .cloned()
        });
        let cached_file = match (cached, signature) {
            (Some(cached), _) => cached,
            (_, Some(signature)) => {
                let events = parse_session_file(file)?;
                CachedTokenUsageFile {
                    len: signature.len,
                    modified_secs: signature.modified_secs,
                    modified_nanos: signature.modified_nanos,
                    events,
                }
            }
            (None, None) => {
                let events = parse_session_file(file)?;
                CachedTokenUsageFile {
                    events,
                    ..CachedTokenUsageFile::default()
                }
            }
        };
        all_events.extend(cached_file.events.clone());
        next_files.insert(cache_key, cached_file);
    }

    cache.version = TOKEN_USAGE_CACHE_VERSION;
    cache.files = next_files;
    let _ = write_token_usage_cache(&cache_path, &cache);

    let pricing_path = default_pricing_override_path(&cache_dir);
    let catalog = PricingCatalog::builtin().load_override(&pricing_path)?;
    Ok(summarize_token_events(
        codex_dir,
        files.len(),
        all_events,
        &catalog,
    ))
}

fn summarize_token_events(
    codex_dir: PathBuf,
    files_scanned: usize,
    events: Vec<TokenUsageEvent>,
    pricing: &PricingCatalog,
) -> TokenUsageSummary {
    let mut summary = TokenUsageSummary {
        codex_dir,
        files_scanned,
        pricing_as_of: PRICING_AS_OF.to_string(),
        pricing_override_path: pricing.override_path.clone(),
        ..TokenUsageSummary::default()
    };
    let mut summary_cost = CostBreakdown::default();
    let mut sessions = BTreeSet::new();
    let mut unpriced_models = BTreeSet::new();
    let mut by_day = BTreeMap::<String, TokenUsageBucketAccumulator>::new();
    let mut by_model = BTreeMap::<String, TokenUsageBucketAccumulator>::new();
    let mut by_provider = BTreeMap::<String, TokenUsageBucketAccumulator>::new();
    let mut seen_events = BTreeSet::<String>::new();

    for mut event in events {
        if !seen_events.insert(token_event_fingerprint(&event)) {
            continue;
        }
        if let Some(session_id) = event.session_id.as_ref() {
            sessions.insert(session_id.clone());
        }
        summary.events += 1;
        summary.input_tokens += event.input_tokens;
        summary.cached_input_tokens += event.cached_input_tokens;
        summary.output_tokens += event.output_tokens;
        summary.total_tokens += event.total_tokens;
        let event_cost = pricing
            .estimate(
                &event.model,
                event.provider_id.as_deref(),
                event.input_tokens,
                event.cached_input_tokens,
                event.output_tokens,
            )
            .map(|(matched, cost)| {
                event.pricing_model = Some(matched.model.clone());
                event.cost = Some(cost.to_api());
                cost
            });
        if let Some(cost) = event_cost.as_ref() {
            summary.priced_events += 1;
            summary_cost.add_assign(cost);
        } else {
            summary.unpriced_events += 1;
            unpriced_models.insert(event.model.clone());
        }

        let day = day_key(event.timestamp.as_deref());
        add_to_bucket(
            by_day.entry(day.clone()).or_insert_with(|| bucket(&day)),
            &event,
            event_cost.as_ref(),
        );
        add_to_bucket(
            by_model
                .entry(event.model.clone())
                .or_insert_with(|| bucket(&event.model)),
            &event,
            event_cost.as_ref(),
        );
        let provider_id = event.provider_id.as_deref().unwrap_or("unknown");
        add_to_bucket(
            by_provider
                .entry(provider_id.to_string())
                .or_insert_with(|| bucket(provider_id)),
            &event,
            event_cost.as_ref(),
        );
        summary.recent_events.push(event);
    }

    summary.sessions = sessions.len();
    summary.cost = summary_cost.to_api();
    summary.unpriced_models = unpriced_models.into_iter().collect();
    summary.by_day = buckets_desc(by_day, false);
    summary.by_model = buckets_desc(by_model, true);
    summary.by_provider = buckets_desc(by_provider, true);
    summary.recent_events.sort_by(|left, right| {
        left.timestamp
            .as_deref()
            .unwrap_or("")
            .cmp(right.timestamp.as_deref().unwrap_or(""))
    });
    if summary.recent_events.len() > 20 {
        summary.recent_events = summary
            .recent_events
            .split_off(summary.recent_events.len() - 20);
    }
    summary
}

fn parse_session_file(path: &Path) -> Result<Vec<TokenUsageEvent>> {
    let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    let reader = BufReader::new(file);
    let mut state = FileParseState::default();
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        if !line.contains("token_count")
            && !line.contains("turn_context")
            && !line.contains("session_meta")
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "session_meta" => apply_session_meta(&mut state, &value),
            "turn_context" => apply_turn_context(&mut state, &value),
            "event_msg" => {
                if let Some(event) = parse_token_event(&mut state, &value) {
                    events.push(event);
                }
            }
            _ => {}
        }
    }

    Ok(events)
}

#[derive(Debug, Clone)]
struct FileSignature {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

impl CachedTokenUsageFile {
    fn matches(&self, signature: &FileSignature) -> bool {
        self.len == signature.len
            && self.modified_secs == signature.modified_secs
            && self.modified_nanos == signature.modified_nanos
    }
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(FileSignature {
        len: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    })
}

fn read_token_usage_cache(path: &Path) -> TokenUsageCache {
    let Ok(text) = fs::read_to_string(path) else {
        return TokenUsageCache {
            version: TOKEN_USAGE_CACHE_VERSION,
            ..TokenUsageCache::default()
        };
    };
    let Ok(cache) = serde_json::from_str::<TokenUsageCache>(&text) else {
        return TokenUsageCache {
            version: TOKEN_USAGE_CACHE_VERSION,
            ..TokenUsageCache::default()
        };
    };
    if cache.version == TOKEN_USAGE_CACHE_VERSION {
        cache
    } else {
        TokenUsageCache {
            version: TOKEN_USAGE_CACHE_VERSION,
            ..TokenUsageCache::default()
        }
    }
}

fn write_token_usage_cache(path: &Path, cache: &TokenUsageCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }
    let text = serde_json::to_string(cache).map_err(|source| {
        CompanionError::InvalidConfig(format!("token usage cache serialize failed: {source}"))
    })?;
    fs::write(path, text).map_err(|source| CompanionError::io(path, source))
}

fn apply_session_meta(state: &mut FileParseState, value: &Value) {
    let Some(payload) = value.get("payload") else {
        return;
    };
    if state.session_id.is_none() {
        state.session_id = pick_string(payload, &[&["session_id"], &["sessionId"], &["id"]]);
    }
    update_model_and_provider(state, payload);
}

fn apply_turn_context(state: &mut FileParseState, value: &Value) {
    if let Some(payload) = value.get("payload") {
        update_model_and_provider(state, payload);
    }
}

fn parse_token_event(state: &mut FileParseState, value: &Value) -> Option<TokenUsageEvent> {
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info").filter(|info| !info.is_null())?;
    update_model_and_provider(state, info);
    update_model_and_provider(state, payload);

    let delta = token_event_delta(state, info)?;
    let delta = DeltaTokens {
        cached_input: delta.cached_input.min(delta.input),
        ..delta
    };
    if delta.is_zero() {
        return None;
    }

    Some(TokenUsageEvent {
        timestamp: pick_string(value, &[&["timestamp"]]),
        session_id: state.session_id.clone(),
        model: state.current_model.clone(),
        provider_id: state.current_provider_id.clone(),
        input_tokens: delta.fresh_input(),
        cached_input_tokens: delta.cached_input,
        output_tokens: delta.output,
        total_tokens: delta.total(),
        cost: None,
        pricing_model: None,
    })
}

fn token_event_delta(state: &mut FileParseState, info: &Value) -> Option<DeltaTokens> {
    let total_tokens = info
        .get("total_token_usage")
        .and_then(parse_cumulative_tokens);
    let last_tokens = info
        .get("last_token_usage")
        .and_then(parse_cumulative_tokens);

    if let Some(last) = last_tokens {
        if let Some(total) = total_tokens {
            state.prev_total = Some(total);
        }
        return Some(DeltaTokens {
            input: last.input,
            cached_input: last.cached_input,
            output: last.output,
        });
    }

    let total = total_tokens?;
    let delta = compute_delta(&state.prev_total, &total);
    state.prev_total = Some(total);
    Some(delta)
}

fn update_model_and_provider(state: &mut FileParseState, value: &Value) {
    if let Some(model) = pick_string(
        value,
        &[
            &["model"],
            &["model_name"],
            &["modelName"],
            &["info", "model"],
            &["payload", "model"],
        ],
    ) {
        state.current_model = normalize_codex_model(&model);
    }
    if let Some(provider_id) = pick_string(
        value,
        &[
            &["model_provider"],
            &["modelProvider"],
            &["provider_id"],
            &["providerId"],
            &["provider"],
            &["info", "model_provider"],
            &["info", "provider_id"],
        ],
    ) {
        state.current_provider_id = Some(provider_id);
    }
}

fn parse_cumulative_tokens(value: &Value) -> Option<CumulativeTokens> {
    if !value.is_object() {
        return None;
    }
    Some(CumulativeTokens {
        input: pick_u64(
            value,
            &[
                &["input_tokens"],
                &["inputTokens"],
                &["prompt_tokens"],
                &["promptTokens"],
            ],
        )
        .unwrap_or(0),
        cached_input: pick_u64(
            value,
            &[
                &["cached_input_tokens"],
                &["cachedInputTokens"],
                &["cache_read_input_tokens"],
                &["cacheReadInputTokens"],
            ],
        )
        .unwrap_or(0),
        output: pick_u64(
            value,
            &[
                &["output_tokens"],
                &["outputTokens"],
                &["completion_tokens"],
                &["completionTokens"],
            ],
        )
        .unwrap_or(0),
    })
}

fn compute_delta(prev: &Option<CumulativeTokens>, current: &CumulativeTokens) -> DeltaTokens {
    match prev {
        None => DeltaTokens {
            input: current.input,
            cached_input: current.cached_input,
            output: current.output,
        },
        Some(prev) => DeltaTokens {
            input: current.input.saturating_sub(prev.input),
            cached_input: current.cached_input.saturating_sub(prev.cached_input),
            output: current.output.saturating_sub(prev.output),
        },
    }
}

fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_jsonl_recursive(&codex_dir.join("sessions"), &mut files, 0, 4);
    collect_jsonl_recursive(&codex_dir.join("archived_sessions"), &mut files, 0, 1);
    files.sort();
    files
}

fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth);
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

#[derive(Debug, Default)]
struct TokenUsageBucketAccumulator {
    bucket: TokenUsageBucket,
    cost: CostBreakdown,
}

impl TokenUsageBucketAccumulator {
    fn finish(mut self) -> TokenUsageBucket {
        self.bucket.cost = self.cost.to_api();
        self.bucket
    }
}

fn add_to_bucket(
    accumulator: &mut TokenUsageBucketAccumulator,
    event: &TokenUsageEvent,
    cost: Option<&CostBreakdown>,
) {
    accumulator.bucket.events += 1;
    accumulator.bucket.input_tokens += event.input_tokens;
    accumulator.bucket.cached_input_tokens += event.cached_input_tokens;
    accumulator.bucket.output_tokens += event.output_tokens;
    accumulator.bucket.total_tokens += event.total_tokens;
    if let Some(cost) = cost {
        accumulator.bucket.priced_events += 1;
        accumulator.cost.add_assign(cost);
    } else {
        accumulator.bucket.unpriced_events += 1;
    }
}

fn day_key(timestamp: Option<&str>) -> String {
    timestamp
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .or_else(|| {
            timestamp
                .and_then(|value| value.get(..10))
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "未知日期".to_string())
}

fn token_event_fingerprint(event: &TokenUsageEvent) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        event.session_id.as_deref().unwrap_or(""),
        event.timestamp.as_deref().unwrap_or(""),
        event.provider_id.as_deref().unwrap_or(""),
        event.model,
        event.input_tokens,
        event.cached_input_tokens,
        event.output_tokens
    )
}

fn bucket(key: &str) -> TokenUsageBucketAccumulator {
    TokenUsageBucketAccumulator {
        bucket: TokenUsageBucket {
            key: key.to_string(),
            ..TokenUsageBucket::default()
        },
        ..TokenUsageBucketAccumulator::default()
    }
}

fn buckets_desc(
    map: BTreeMap<String, TokenUsageBucketAccumulator>,
    by_total: bool,
) -> Vec<TokenUsageBucket> {
    let mut buckets = map
        .into_values()
        .map(TokenUsageBucketAccumulator::finish)
        .collect::<Vec<_>>();
    if by_total {
        buckets.sort_by(|left, right| {
            right
                .total_tokens
                .cmp(&left.total_tokens)
                .then_with(|| left.key.cmp(&right.key))
        });
    } else {
        buckets.sort_by(|left, right| right.key.cmp(&left.key));
    }
    buckets
}

fn normalize_codex_model(raw: &str) -> String {
    let mut name = raw.trim().to_ascii_lowercase();
    if let Some((_, suffix)) = name.rsplit_once('/') {
        name = suffix.to_string();
    }
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        if suffix.as_bytes().first() == Some(&b'-')
            && suffix[1..5].chars().all(|value| value.is_ascii_digit())
            && suffix.as_bytes().get(5) == Some(&b'-')
            && suffix[6..8].chars().all(|value| value.is_ascii_digit())
            && suffix.as_bytes().get(8) == Some(&b'-')
            && suffix[9..11].chars().all(|value| value.is_ascii_digit())
        {
            name.truncate(name.len() - 11);
        }
    }
    if name.len() > 9 {
        if let Some((head, tail)) = name.rsplit_once('-') {
            if tail.len() == 8 && tail.chars().all(|value| value.is_ascii_digit()) {
                name = head.to_string();
            }
        }
    }
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

fn pick_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut cursor = value;
        let mut found = true;
        for key in *path {
            match cursor.get(*key) {
                Some(next) => cursor = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            if let Some(text) = cursor
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn pick_u64(value: &Value, paths: &[&[&str]]) -> Option<u64> {
    for path in paths {
        let mut cursor = value;
        let mut found = true;
        for key in *path {
            match cursor.get(*key) {
                Some(next) => cursor = next,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            if let Some(number) = cursor.as_u64() {
                return Some(number);
            }
            if let Some(number) = cursor.as_str().and_then(|text| text.parse::<u64>().ok()) {
                return Some(number);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_delta_from_cumulative_token_count() {
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 40,
            output: 10,
        });
        let current = CumulativeTokens {
            input: 160,
            cached_input: 80,
            output: 30,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 60);
        assert_eq!(delta.cached_input, 40);
        assert_eq!(delta.output, 20);
    }

    #[test]
    fn scans_codex_session_jsonl() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("07");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("session.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"codex-companion"}}"#.to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"openai/gpt-5.3-codex-2026-06-07"}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"cached_input_tokens":30,"output_tokens":30}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:02:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"cached_input_tokens":30,"output_tokens":30}}}}"#
                + "\n",
        )
        .expect("write");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.files_scanned, 1);
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.events, 2);
        assert_eq!(summary.input_tokens, 130);
        assert_eq!(summary.cached_input_tokens, 30);
        assert_eq!(summary.output_tokens, 30);
        assert_eq!(summary.total_tokens, 190);
        assert_eq!(summary.by_model[0].key, "gpt-5.3-codex");
        assert_eq!(summary.by_provider[0].key, "codex-companion");
        assert_eq!(summary.priced_events, 2);
        assert_eq!(summary.unpriced_events, 0);
        assert_eq!(summary.cost.fresh_input_usd, "0.0002275");
        assert_eq!(summary.cost.cached_input_usd, "0.00000525");
        assert_eq!(summary.cost.output_usd, "0.00042");
        assert_eq!(summary.cost.total_usd, "0.00065275");
    }

    #[test]
    fn cached_scan_invalidates_changed_files() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("07");
        fs::create_dir_all(&day).expect("mkdir");
        let session = day.join("session.jsonl");
        let base =
            r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"codex-companion"}}"#
                .to_string()
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}}}"#
                + "\n";
        fs::write(&session, &base).expect("write");

        let cache_dir = temp.path().join("cache");
        let first = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("first");
        let second = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("second");
        assert_eq!(first.total_tokens, 110);
        assert_eq!(second.total_tokens, first.total_tokens);
        assert!(cache_dir.join("token-usage-cache.json").exists());

        fs::write(
            &session,
            base + r#"{"timestamp":"2026-06-07T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20,"cached_input_tokens":0,"output_tokens":5}}}}"#
                + "\n",
        )
        .expect("rewrite");
        let third =
            collect_token_usage_cached(temp.path().to_path_buf(), cache_dir).expect("third");
        assert_eq!(third.events, 2);
        assert_eq!(third.total_tokens, 135);
    }

    #[test]
    fn dedupes_archived_copy_of_session_events() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("07");
        fs::create_dir_all(&day).expect("mkdir sessions");
        let archived = temp.path().join("archived_sessions");
        fs::create_dir_all(&archived).expect("mkdir archived");
        let text =
            r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"codex-companion"}}"#
                .to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}}}"#
                + "\n";
        fs::write(day.join("session.jsonl"), &text).expect("write sessions");
        fs::write(archived.join("session.jsonl"), &text).expect("write archived");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.files_scanned, 2);
        assert_eq!(summary.events, 1);
        assert_eq!(summary.input_tokens, 60);
        assert_eq!(summary.cached_input_tokens, 40);
        assert_eq!(summary.total_tokens, 110);
    }

    #[test]
    fn falls_back_to_last_usage_when_cumulative_regresses() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("07");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("session.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"codex-companion"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"cached_input_tokens":100,"output_tokens":20},"last_token_usage":{"input_tokens":200,"cached_input_tokens":100,"output_tokens":20}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":20,"output_tokens":5},"last_token_usage":{"input_tokens":50,"cached_input_tokens":20,"output_tokens":5}}}}"#
                + "\n",
        )
        .expect("write");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.events, 2);
        assert_eq!(summary.input_tokens, 130);
        assert_eq!(summary.cached_input_tokens, 120);
        assert_eq!(summary.output_tokens, 25);
        assert_eq!(summary.total_tokens, 275);
    }

    #[test]
    fn prefers_last_usage_over_cumulative_total_for_recent_events() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("07");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("session.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"s1","model_provider":"openai"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":10},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-06-07T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100000,"cached_input_tokens":5000,"output_tokens":1000},"last_token_usage":{"input_tokens":500,"cached_input_tokens":100,"output_tokens":20}}}}"#
                + "\n",
        )
        .expect("write");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        let latest = summary.recent_events.last().expect("latest");
        assert_eq!(latest.input_tokens, 400);
        assert_eq!(latest.cached_input_tokens, 100);
        assert_eq!(latest.output_tokens, 20);
        assert_eq!(latest.total_tokens, 520);
    }
}
