use crate::pricing::{default_pricing_override_path, CostBreakdown, PricingCatalog, PRICING_AS_OF};
use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use codex_companion_core::{
    CompanionError, Result, TokenUsageBucket, TokenUsageEvent, TokenUsagePricingSource,
    TokenUsageSummary, TokenUsageSyncStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const TOKEN_USAGE_CACHE_VERSION: u32 = 9;
const CODEX_AUTO_REVIEW_MODEL: &str = "codex-auto-review";
const TOKEN_USAGE_PREFIX_BYTES: u64 = 64 * 1024;

static TOKEN_USAGE_SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static TOKEN_USAGE_STATUS: OnceLock<Mutex<TokenUsageSyncStatus>> = OnceLock::new();

pub fn token_usage_sync_status() -> TokenUsageSyncStatus {
    TOKEN_USAGE_STATUS
        .get_or_init(|| Mutex::new(TokenUsageSyncStatus::default()))
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsageDateRange {
    start_at: Option<DateTime<Utc>>,
    end_at: Option<DateTime<Utc>>,
}

impl TokenUsageDateRange {
    pub fn parse(start_date: Option<&str>, end_date: Option<&str>) -> Result<Self> {
        let start_at = parse_date_boundary("开始时间", start_date, DateBoundary::Start)?;
        let end_at = parse_date_boundary("结束时间", end_date, DateBoundary::End)?;
        if start_at.zip(end_at).is_some_and(|(start, end)| start > end) {
            return Err(CompanionError::InvalidConfig(
                "开始时间不能晚于结束时间".into(),
            ));
        }
        Ok(Self { start_at, end_at })
    }

    fn includes(&self, timestamp: Option<&str>) -> bool {
        if self.start_at.is_none() && self.end_at.is_none() {
            return true;
        }
        let Some(timestamp) = timestamp.and_then(event_timestamp) else {
            return false;
        };
        self.start_at.is_none_or(|start| timestamp >= start)
            && self.end_at.is_none_or(|end| timestamp <= end)
    }
}

#[derive(Debug, Clone, Copy)]
enum DateBoundary {
    Start,
    End,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsageFilters {
    provider_id: Option<String>,
    model: Option<String>,
}

impl TokenUsageFilters {
    pub fn parse(provider_id: Option<&str>, model: Option<&str>) -> Self {
        Self {
            provider_id: normalize_filter_value(provider_id),
            model: normalize_filter_value(model),
        }
    }

    fn includes(&self, event: &TokenUsageEvent) -> bool {
        self.matches_provider(event)
            && self
                .model
                .as_deref()
                .is_none_or(|model| event.model == model)
    }

    fn matches_provider(&self, event: &TokenUsageEvent) -> bool {
        let provider_id = event.provider_id.as_deref().unwrap_or("unknown");
        self.provider_id
            .as_deref()
            .is_none_or(|selected| provider_id == selected)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

#[derive(Debug, Clone, Default)]
struct DeltaTokens {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.cache_write_input == 0 && self.output == 0
    }

    fn fresh_input(&self) -> u64 {
        self.input
            .saturating_sub(self.cached_input)
            .saturating_sub(self.cache_write_input)
    }

    fn total(&self) -> u64 {
        self.fresh_input() + self.cached_input + self.cache_write_input + self.output
    }
}

#[derive(Debug, Clone)]
struct FileParseState {
    session_id: Option<String>,
    current_model: String,
    current_provider_id: Option<String>,
    prev_total: Option<CumulativeTokens>,
    replay_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedParseState {
    session_id: Option<String>,
    current_model: String,
    current_provider_id: Option<String>,
    prev_total: Option<CumulativeTokens>,
    replay_sensitive: bool,
}

impl From<&FileParseState> for CachedParseState {
    fn from(state: &FileParseState) -> Self {
        Self {
            session_id: state.session_id.clone(),
            current_model: state.current_model.clone(),
            current_provider_id: state.current_provider_id.clone(),
            prev_total: state.prev_total.clone(),
            replay_sensitive: state.replay_sensitive,
        }
    }
}

impl CachedParseState {
    fn into_file_parse_state(self) -> FileParseState {
        FileParseState {
            session_id: self.session_id,
            current_model: self.current_model,
            current_provider_id: self.current_provider_id,
            prev_total: self.prev_total,
            replay_sensitive: self.replay_sensitive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexSessionIdentity {
    thread_id: String,
    parent_thread_id: Option<String>,
    forked_at: Option<DateTime<Utc>>,
    carries_history_snapshot: bool,
}

#[derive(Debug, Clone)]
struct ParsedTokenUsageEvent {
    event: TokenUsageEvent,
    signature: CumulativeTokens,
    line_index: usize,
}

impl Default for FileParseState {
    fn default() -> Self {
        Self {
            session_id: None,
            current_model: "unknown".to_string(),
            current_provider_id: None,
            prev_total: None,
            replay_sensitive: false,
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
    #[serde(default)]
    deferred: bool,
    #[serde(default)]
    suspected_duplicate: bool,
    #[serde(default)]
    parsed_bytes: u64,
    #[serde(default)]
    parsed_lines: usize,
    #[serde(default)]
    prefix_digest: String,
    #[serde(default)]
    parser_state: Option<CachedParseState>,
    #[serde(default)]
    replay_sensitive: bool,
}

#[derive(Debug, Clone, Default)]
struct ParsedTokenUsageFile {
    events: Vec<TokenUsageEvent>,
    deferred: bool,
    suspected_duplicate: bool,
    parsed_bytes: u64,
    parsed_lines: usize,
    prefix_digest: String,
    parser_state: Option<CachedParseState>,
    replay_sensitive: bool,
}

#[derive(Debug)]
struct ReplayParentCatalog {
    files_by_thread: BTreeMap<String, Vec<PathBuf>>,
    identities_by_thread: BTreeMap<String, Vec<CodexSessionIdentity>>,
    timelines: Mutex<BTreeMap<PathBuf, Option<Arc<ReplayParentTimeline>>>>,
}

#[derive(Debug, Default)]
struct ReplayParentTimeline {
    max_timestamp: Option<DateTime<Utc>>,
    terminal_timestamps: Vec<DateTime<Utc>>,
    signatures: Vec<(DateTime<Utc>, CumulativeTokens)>,
    models: Vec<(DateTime<Utc>, String)>,
}

impl ReplayParentCatalog {
    fn new(files: Vec<PathBuf>) -> Self {
        let mut files_by_thread = BTreeMap::<String, Vec<PathBuf>>::new();
        let mut identities_by_thread = BTreeMap::<String, Vec<CodexSessionIdentity>>::new();
        for path in files {
            let Some(identity) = read_first_session_identity(&path).ok().flatten() else {
                continue;
            };
            let thread_id = identity.thread_id.clone();
            files_by_thread
                .entry(thread_id.clone())
                .or_default()
                .push(path);
            identities_by_thread
                .entry(thread_id)
                .or_default()
                .push(identity);
        }
        Self {
            files_by_thread,
            identities_by_thread,
            timelines: Mutex::new(BTreeMap::new()),
        }
    }

    fn candidates_for(&self, thread_id: &str) -> Vec<PathBuf> {
        self.files_by_thread
            .get(thread_id)
            .cloned()
            .unwrap_or_default()
    }

    fn timeline_for(&self, path: &Path) -> Option<Arc<ReplayParentTimeline>> {
        let mut timelines = self.timelines.lock().ok()?;
        if let Some(cached) = timelines.get(path) {
            return cached.clone();
        }
        let loaded = read_replay_parent_timeline(path).ok().map(Arc::new);
        timelines.insert(path.to_path_buf(), loaded.clone());
        loaded
    }

    fn model_for_thread_at(&self, thread_id: &str, cutoff: DateTime<Utc>) -> Option<String> {
        let models = self
            .candidates_for(thread_id)
            .into_iter()
            .filter_map(|path| self.timeline_for(&path))
            .filter_map(|timeline| parent_model_before_timeline(&timeline, cutoff))
            .collect::<BTreeSet<_>>();
        if models.len() == 1 {
            models.into_iter().next()
        } else {
            None
        }
    }

    fn inherited_model_for_session(&self, thread_id: &str) -> Option<String> {
        let identities = self.identities_by_thread.get(thread_id)?;
        let mut contexts = BTreeSet::new();
        for identity in identities {
            let (Some(parent_thread_id), Some(forked_at)) =
                (identity.parent_thread_id.as_deref(), identity.forked_at)
            else {
                return None;
            };
            contexts.insert((parent_thread_id.to_string(), forked_at));
        }
        if contexts.len() != 1 {
            return None;
        }
        let (parent_thread_id, forked_at) = contexts.into_iter().next()?;
        self.model_for_thread_at(&parent_thread_id, forked_at)
    }
}

struct TokenUsageSummaryInput {
    codex_dir: PathBuf,
    files_scanned: usize,
    deferred_files: usize,
    suspected_duplicates: usize,
    events: Vec<TokenUsageEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayResolution {
    NotApplicable,
    Matched(usize),
    Deferred,
    SuspectedDuplicate,
}

pub fn collect_token_usage(codex_dir: PathBuf) -> Result<TokenUsageSummary> {
    let files = collect_codex_session_files(&codex_dir);
    let replay_catalog = ReplayParentCatalog::new(files.clone());
    let mut all_events = Vec::new();
    let mut deferred_files = 0;
    let mut suspected_duplicates = 0;
    for file in &files {
        let parsed = parse_session_file(file, Some(&replay_catalog))?;
        deferred_files += usize::from(parsed.deferred);
        suspected_duplicates += usize::from(parsed.suspected_duplicate);
        all_events.extend(parsed.events);
    }
    apply_inherited_pricing_models(&mut all_events, &replay_catalog);
    let pricing_path = default_pricing_override_path(&codex_dir.join("cache"));
    let catalog = PricingCatalog::builtin().load_override(&pricing_path)?;
    Ok(summarize_token_events(
        TokenUsageSummaryInput {
            codex_dir,
            files_scanned: files.len(),
            deferred_files,
            suspected_duplicates,
            events: all_events,
        },
        &catalog,
        &TokenUsageDateRange::default(),
        &TokenUsageFilters::default(),
    ))
}

pub fn collect_token_usage_cached(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
) -> Result<TokenUsageSummary> {
    collect_token_usage_cached_in_range(codex_dir, cache_dir, &TokenUsageDateRange::default())
}

pub fn collect_token_usage_cached_in_range(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
    date_range: &TokenUsageDateRange,
) -> Result<TokenUsageSummary> {
    collect_token_usage_cached_with_filters(
        codex_dir,
        cache_dir,
        date_range,
        &TokenUsageFilters::default(),
    )
}

pub fn collect_token_usage_cached_with_filters(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
    date_range: &TokenUsageDateRange,
    filters: &TokenUsageFilters,
) -> Result<TokenUsageSummary> {
    let scan_lock = TOKEN_USAGE_SCAN_LOCK.get_or_init(|| Mutex::new(()));
    let _scan_guard = scan_lock
        .lock()
        .map_err(|_| CompanionError::InvalidConfig("token usage scan lock poisoned".into()))?;
    let files = collect_codex_session_files(&codex_dir);
    set_token_usage_status(TokenUsageSyncStatus {
        active: true,
        total_files: files.len(),
        phase: "scanning".to_string(),
        started_at: Some(chrono::Utc::now()),
        ..TokenUsageSyncStatus::default()
    });
    let result = collect_token_usage_cached_inner(codex_dir, cache_dir, date_range, filters, files);
    let mut status = token_usage_sync_status();
    status.active = false;
    status.phase = if result.is_ok() { "complete" } else { "failed" }.to_string();
    status.finished_at = Some(chrono::Utc::now());
    set_token_usage_status(status);
    result
}

fn collect_token_usage_cached_inner(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
    date_range: &TokenUsageDateRange,
    filters: &TokenUsageFilters,
    files: Vec<PathBuf>,
) -> Result<TokenUsageSummary> {
    let cache_path = cache_dir.join("token-usage-cache.json");
    let mut cache = read_token_usage_cache(&cache_path);
    let previous_keys = cache.files.keys().cloned().collect::<BTreeSet<_>>();
    let mut cache_changed = false;
    let mut next_files = BTreeMap::new();
    let mut all_events = Vec::new();
    let mut deferred_files = 0;
    let mut suspected_duplicates = 0;
    let replay_catalog = ReplayParentCatalog::new(files.clone());

    for (file_index, file) in files.iter().enumerate() {
        let cache_key = file.to_string_lossy().to_string();
        let signature = file_signature(file);
        let previous = cache.files.get(&cache_key).cloned();
        let cached_file = match (previous, signature) {
            (Some(cached), Some(signature))
                if cached.matches(&signature)
                    && !cached.deferred
                    && !cached.suspected_duplicate =>
            {
                cached
            }
            (Some(cached), Some(signature)) => {
                cache_changed = true;
                let parsed = match parse_session_file_incremental(file, &cached, &signature)? {
                    Some(parsed) => parsed,
                    None => parse_session_file(file, Some(&replay_catalog))?,
                };
                cached_file_from_parsed(&signature, parsed)
            }
            (_, Some(signature)) => {
                cache_changed = true;
                let parsed = parse_session_file(file, Some(&replay_catalog))?;
                cached_file_from_parsed(&signature, parsed)
            }
            (_, None) => {
                cache_changed = true;
                let parsed = parse_session_file(file, Some(&replay_catalog))?;
                CachedTokenUsageFile {
                    events: parsed.events,
                    deferred: parsed.deferred,
                    suspected_duplicate: parsed.suspected_duplicate,
                    parsed_bytes: parsed.parsed_bytes,
                    parsed_lines: parsed.parsed_lines,
                    prefix_digest: parsed.prefix_digest,
                    parser_state: parsed.parser_state,
                    replay_sensitive: parsed.replay_sensitive,
                    ..CachedTokenUsageFile::default()
                }
            }
        };
        deferred_files += usize::from(cached_file.deferred);
        suspected_duplicates += usize::from(cached_file.suspected_duplicate);
        all_events.extend(cached_file.events.clone());
        next_files.insert(cache_key, cached_file);
        update_token_usage_progress(file_index + 1, deferred_files, suspected_duplicates);
    }

    cache_changed = cache_changed
        || previous_keys.len() != next_files.len()
        || previous_keys
            .iter()
            .any(|cache_key| !next_files.contains_key(cache_key));
    cache.version = TOKEN_USAGE_CACHE_VERSION;
    cache.files = next_files;
    if cache_changed {
        let _ = write_token_usage_cache(&cache_path, &cache);
    }

    apply_inherited_pricing_models(&mut all_events, &replay_catalog);
    let pricing_path = default_pricing_override_path(&cache_dir);
    let catalog = PricingCatalog::builtin().load_override(&pricing_path)?;
    Ok(summarize_token_events(
        TokenUsageSummaryInput {
            codex_dir,
            files_scanned: files.len(),
            deferred_files,
            suspected_duplicates,
            events: all_events,
        },
        &catalog,
        date_range,
        filters,
    ))
}

fn set_token_usage_status(status: TokenUsageSyncStatus) {
    if let Ok(mut current) = TOKEN_USAGE_STATUS
        .get_or_init(|| Mutex::new(TokenUsageSyncStatus::default()))
        .lock()
    {
        *current = status;
    }
}

fn update_token_usage_progress(
    scanned_files: usize,
    deferred_files: usize,
    suspected_duplicates: usize,
) {
    if let Ok(mut status) = TOKEN_USAGE_STATUS
        .get_or_init(|| Mutex::new(TokenUsageSyncStatus::default()))
        .lock()
    {
        status.scanned_files = scanned_files;
        status.deferred_files = deferred_files;
        status.suspected_duplicates = suspected_duplicates;
    }
}

pub fn rebuild_token_usage_cached_with_filters(
    codex_dir: PathBuf,
    cache_dir: PathBuf,
    date_range: &TokenUsageDateRange,
    filters: &TokenUsageFilters,
) -> Result<TokenUsageSummary> {
    let cache_path = cache_dir.join("token-usage-cache.json");
    if let Err(error) = fs::remove_file(&cache_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(CompanionError::io(&cache_path, error));
        }
    }
    collect_token_usage_cached_with_filters(codex_dir, cache_dir, date_range, filters)
}

fn summarize_token_events(
    input: TokenUsageSummaryInput,
    pricing: &PricingCatalog,
    date_range: &TokenUsageDateRange,
    filters: &TokenUsageFilters,
) -> TokenUsageSummary {
    let TokenUsageSummaryInput {
        codex_dir,
        files_scanned,
        deferred_files,
        suspected_duplicates,
        events,
    } = input;
    let mut summary = TokenUsageSummary {
        codex_dir,
        files_scanned,
        deferred_files,
        suspected_duplicates,
        cache_version: TOKEN_USAGE_CACHE_VERSION,
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
    let mut available_providers = BTreeSet::new();
    let mut available_models = BTreeSet::new();

    for mut event in events {
        if !seen_events.insert(token_event_key(&event)) {
            continue;
        }
        if !date_range.includes(event.timestamp.as_deref()) {
            continue;
        }
        let provider_id = event.provider_id.as_deref().unwrap_or("unknown");
        available_providers.insert(provider_id.to_string());
        if filters.matches_provider(&event) {
            available_models.insert(event.model.clone());
        }
        if !filters.includes(&event) {
            continue;
        }
        if let Some(session_id) = event.session_id.as_ref() {
            sessions.insert(session_id.clone());
        }
        summary.events += 1;
        summary.input_tokens += event.input_tokens;
        summary.cached_input_tokens += event.cached_input_tokens;
        summary.cache_write_input_tokens += event.cache_write_input_tokens;
        summary.output_tokens += event.output_tokens;
        summary.total_tokens += event.total_tokens;
        let pricing_model = event.pricing_model.as_deref().unwrap_or(&event.model);
        let inferred_pricing =
            event.pricing_source == Some(TokenUsagePricingSource::InferredParentModel);
        let event_cost = pricing
            .estimate(
                pricing_model,
                event.provider_id.as_deref(),
                event.input_tokens,
                event.cached_input_tokens,
                event.cache_write_input_tokens,
                event.output_tokens,
            )
            .map(|(matched, cost)| {
                event.pricing_model = Some(matched.model.clone());
                event
                    .pricing_source
                    .get_or_insert(TokenUsagePricingSource::EventModel);
                event.cost = Some(cost.to_api());
                cost
            });
        if let Some(cost) = event_cost.as_ref() {
            summary.priced_events += 1;
            if inferred_pricing {
                summary.inferred_priced_events += 1;
            }
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
    summary.available_providers = available_providers.into_iter().collect();
    summary.available_models = available_models.into_iter().collect();
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

fn parse_date_boundary(
    label: &str,
    value: Option<&str>,
    boundary: DateBoundary,
) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(Some(timestamp.with_timezone(&Utc)));
    }
    let naive = parse_local_boundary(value, boundary).ok_or_else(|| {
        CompanionError::InvalidConfig(format!(
            "{label}必须使用 YYYY-MM-DD 或 YYYY-MM-DDTHH:MM[:SS] 格式"
        ))
    })?;
    local_boundary_to_utc(label, naive, boundary).map(Some)
}

fn normalize_filter_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn event_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn parse_local_boundary(value: &str, boundary: DateBoundary) -> Option<NaiveDateTime> {
    let parsed_datetime = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok());
    if let Some(datetime) = parsed_datetime {
        return match boundary {
            DateBoundary::Start => Some(datetime),
            DateBoundary::End => datetime.with_nanosecond(999_999_999),
        };
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    match boundary {
        DateBoundary::Start => date.and_hms_opt(0, 0, 0),
        DateBoundary::End => date.and_hms_nano_opt(23, 59, 59, 999_999_999),
    }
}

fn local_boundary_to_utc(
    label: &str,
    value: NaiveDateTime,
    boundary: DateBoundary,
) -> Result<DateTime<Utc>> {
    let local = match Local.from_local_datetime(&value) {
        LocalResult::Single(timestamp) => timestamp,
        LocalResult::Ambiguous(first, second) => match boundary {
            DateBoundary::Start => first.min(second),
            DateBoundary::End => first.max(second),
        },
        LocalResult::None => {
            return Err(CompanionError::InvalidConfig(format!(
                "{label}在本地时区中不存在"
            )))
        }
    };
    Ok(local.with_timezone(&Utc))
}

fn parse_session_file(
    path: &Path,
    replay_catalog: Option<&ReplayParentCatalog>,
) -> Result<ParsedTokenUsageFile> {
    let (lines, parsed_bytes) = read_complete_lines(path)?;
    let identity = session_identity_from_lines(&lines);
    let fallback_boundary = history_replay_boundary(&lines, identity.as_ref());
    let mut state = FileParseState {
        session_id: identity.as_ref().map(|identity| identity.thread_id.clone()),
        replay_sensitive: identity.as_ref().is_some_and(|identity| {
            identity.parent_thread_id.is_some() || identity.carries_history_snapshot
        }),
        ..FileParseState::default()
    };
    let parsed_events = parse_token_lines(&lines, &mut state, 0);
    let metadata = parsed_file_metadata(path, parsed_bytes, lines.len(), &state);

    let replay_resolution = identity
        .as_ref()
        .map_or(ReplayResolution::NotApplicable, |identity| {
            replay_prefix_from_parent(path, &lines, identity, &parsed_events, replay_catalog)
        });
    if matches!(replay_resolution, ReplayResolution::Deferred) {
        return Ok(ParsedTokenUsageFile {
            deferred: true,
            ..metadata
        });
    }
    if matches!(replay_resolution, ReplayResolution::SuspectedDuplicate) {
        return Ok(ParsedTokenUsageFile {
            suspected_duplicate: true,
            ..metadata
        });
    }
    let events = parsed_events
        .into_iter()
        .enumerate()
        .filter(|(event_index, parsed)| match replay_resolution {
            ReplayResolution::Matched(prefix) => *event_index >= prefix,
            ReplayResolution::NotApplicable => {
                fallback_boundary.is_none_or(|boundary| parsed.line_index + 1 >= boundary)
            }
            ReplayResolution::Deferred | ReplayResolution::SuspectedDuplicate => false,
        })
        .map(|(_, parsed)| parsed.event)
        .collect::<Vec<_>>();
    Ok(ParsedTokenUsageFile { events, ..metadata })
}

fn apply_inherited_pricing_models(
    events: &mut [TokenUsageEvent],
    replay_catalog: &ReplayParentCatalog,
) {
    for event in events
        .iter_mut()
        .filter(|event| event.model == CODEX_AUTO_REVIEW_MODEL)
    {
        event.pricing_model = None;
        event.pricing_source = None;
        let Some(session_id) = event.session_id.as_deref() else {
            continue;
        };
        let Some(parent_model) = replay_catalog.inherited_model_for_session(session_id) else {
            continue;
        };
        if parent_model == CODEX_AUTO_REVIEW_MODEL {
            continue;
        }
        event.pricing_model = Some(parent_model);
        event.pricing_source = Some(TokenUsagePricingSource::InferredParentModel);
    }
}

fn parse_token_lines(
    lines: &[String],
    state: &mut FileParseState,
    line_offset: usize,
) -> Vec<ParsedTokenUsageEvent> {
    let mut parsed_events = Vec::new();
    for (relative_line_index, line) in lines.iter().enumerate() {
        if !line.contains("token_count")
            && !line.contains("turn_context")
            && !line.contains("session_meta")
        {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            continue;
        };
        let line_index = line_offset + relative_line_index;
        match kind {
            "session_meta" => apply_session_meta(state, &value),
            "turn_context" => apply_turn_context(state, &value),
            "event_msg" => {
                if let Some(mut event) = parse_token_event(state, &value) {
                    event.event_id = Some(stable_event_id(&event, line_index));
                    if let Some(signature) = token_usage_signature(&value) {
                        parsed_events.push(ParsedTokenUsageEvent {
                            event,
                            signature,
                            line_index,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    parsed_events
}

fn parsed_file_metadata(
    path: &Path,
    parsed_bytes: u64,
    parsed_lines: usize,
    state: &FileParseState,
) -> ParsedTokenUsageFile {
    ParsedTokenUsageFile {
        parsed_bytes,
        parsed_lines,
        prefix_digest: file_prefix_digest(path).unwrap_or_default(),
        parser_state: Some(CachedParseState::from(state)),
        replay_sensitive: state.replay_sensitive,
        ..ParsedTokenUsageFile::default()
    }
}

fn cached_file_from_parsed(
    signature: &FileSignature,
    parsed: ParsedTokenUsageFile,
) -> CachedTokenUsageFile {
    CachedTokenUsageFile {
        len: signature.len,
        modified_secs: signature.modified_secs,
        modified_nanos: signature.modified_nanos,
        events: parsed.events,
        deferred: parsed.deferred,
        suspected_duplicate: parsed.suspected_duplicate,
        parsed_bytes: parsed.parsed_bytes,
        parsed_lines: parsed.parsed_lines,
        prefix_digest: parsed.prefix_digest,
        parser_state: parsed.parser_state,
        replay_sensitive: parsed.replay_sensitive,
    }
}

fn parse_session_file_incremental(
    path: &Path,
    cached: &CachedTokenUsageFile,
    signature: &FileSignature,
) -> Result<Option<ParsedTokenUsageFile>> {
    if cached.deferred
        || cached.suspected_duplicate
        || cached.replay_sensitive
        || cached.parser_state.is_none()
        || signature.len <= cached.parsed_bytes
        || cached.prefix_digest.is_empty()
    {
        return Ok(None);
    }
    if file_prefix_digest(path).as_deref() != Some(cached.prefix_digest.as_str())
        || !file_ends_with_newline(path)
    {
        return Ok(None);
    }

    let mut file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    file.seek(SeekFrom::Start(cached.parsed_bytes))
        .map_err(|source| CompanionError::io(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| CompanionError::io(path, source))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().map(str::to_string).collect::<Vec<_>>();
    let mut state = cached
        .parser_state
        .clone()
        .expect("parser state checked above")
        .into_file_parse_state();
    let parsed_tail = parse_token_lines(&lines, &mut state, cached.parsed_lines);
    let mut events = cached.events.clone();
    events.extend(parsed_tail.into_iter().map(|parsed| parsed.event));
    let parsed_bytes = cached.parsed_bytes + bytes.len() as u64;
    let metadata = parsed_file_metadata(
        path,
        parsed_bytes,
        cached.parsed_lines + lines.len(),
        &state,
    );
    Ok(Some(ParsedTokenUsageFile { events, ..metadata }))
}

fn read_complete_lines(path: &Path) -> Result<(Vec<String>, u64)> {
    let bytes = fs::read(path).map_err(|source| CompanionError::io(path, source))?;
    let parsed_bytes = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index as u64 + 1);
    let lines = String::from_utf8_lossy(&bytes[..parsed_bytes as usize])
        .lines()
        .map(str::to_string)
        .collect();
    Ok((lines, parsed_bytes))
}

fn file_ends_with_newline(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() == 0 {
        return false;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::End(-1)).is_err() {
        return false;
    }
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).is_ok() && byte[0] == b'\n'
}

fn file_prefix_digest(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = fs::metadata(path).ok()?.len().min(TOKEN_USAGE_PREFIX_BYTES);
    let mut bytes = vec![0_u8; len as usize];
    file.read_exact(&mut bytes).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

fn read_first_session_identity(path: &Path) -> Result<Option<CodexSessionIdentity>> {
    let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| CompanionError::io(path, source))?;
        if bytes_read == 0 || line.last() != Some(&b'\n') {
            return Ok(None);
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return Ok(value.get("payload").and_then(|payload| {
            parse_codex_session_identity(payload, value.get("timestamp").and_then(Value::as_str))
        }));
    }
}

fn read_replay_parent_timeline(path: &Path) -> Result<ReplayParentTimeline> {
    let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut timeline = ReplayParentTimeline::default();
    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| CompanionError::io(path, source))?;
        if bytes_read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        append_replay_parent_timeline_value(&mut timeline, &value);
    }
    Ok(timeline)
}

fn session_identity_from_lines(lines: &[String]) -> Option<CodexSessionIdentity> {
    session_identities_from_lines(lines).into_iter().next()
}

fn session_identities_from_lines(lines: &[String]) -> Vec<CodexSessionIdentity> {
    lines
        .iter()
        .filter_map(|line| {
            if !line.contains("session_meta") {
                return None;
            }
            let value = serde_json::from_str::<Value>(line).ok()?;
            if value.get("type").and_then(Value::as_str) != Some("session_meta") {
                return None;
            }
            value.get("payload").and_then(|payload| {
                parse_codex_session_identity(
                    payload,
                    value.get("timestamp").and_then(Value::as_str),
                )
            })
        })
        .collect()
}

fn parse_codex_session_identity(
    payload: &Value,
    timestamp: Option<&str>,
) -> Option<CodexSessionIdentity> {
    let thread_id = pick_string(
        payload,
        &[
            &["id"],
            &["thread_id"],
            &["threadId"],
            &["session_id"],
            &["sessionId"],
        ],
    )?;
    let forked_from_id = pick_string(payload, &[&["forked_from_id"]]);
    let spawned_from_id = pick_string(
        payload,
        &[&["source", "subagent", "thread_spawn", "parent_thread_id"]],
    );
    let explicit_parent_id = pick_string(payload, &[&["parent_thread_id"], &["parentThreadId"]]);
    let parent_thread_ids = [forked_from_id, spawned_from_id, explicit_parent_id]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    let legacy_parent = pick_string(payload, &[&["session_id"], &["sessionId"]])
        .filter(|parent| parent != &thread_id);
    let parent_thread_id = match parent_thread_ids.len() {
        0 => legacy_parent,
        1 => parent_thread_ids.into_iter().next(),
        _ => None,
    };
    let carries_history_snapshot = parent_thread_id.is_some()
        || payload
            .get("forked_from_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        || payload
            .get("source")
            .and_then(|source| source.get("subagent"))
            .is_some();
    Some(CodexSessionIdentity {
        thread_id,
        parent_thread_id,
        forked_at: timestamp
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        carries_history_snapshot,
    })
}

fn token_usage_signature(value: &Value) -> Option<CumulativeTokens> {
    let info = value.pointer("/payload/info")?.as_object()?;
    info.get("total_token_usage")
        .or_else(|| info.get("last_token_usage"))
        .and_then(parse_cumulative_tokens)
}

fn is_session_terminal_event(value: &Value) -> bool {
    let event_type = match value.get("type").and_then(Value::as_str) {
        Some("event_msg") => value.pointer("/payload/type").and_then(Value::as_str),
        Some(event_type) => Some(event_type),
        None => value.pointer("/payload/type").and_then(Value::as_str),
    };
    matches!(
        event_type,
        Some("task_complete") | Some("turn_aborted") | Some("thread_rolled_back")
    )
}

fn replay_prefix_from_parent(
    child_path: &Path,
    child_lines: &[String],
    identity: &CodexSessionIdentity,
    child_events: &[ParsedTokenUsageEvent],
    replay_catalog: Option<&ReplayParentCatalog>,
) -> ReplayResolution {
    let Some(parent_id) = identity.parent_thread_id.as_deref() else {
        return ReplayResolution::NotApplicable;
    };
    let Some(cutoff) = identity.forked_at else {
        return ReplayResolution::NotApplicable;
    };
    let Some(codex_dir) = child_path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        (name == "sessions" || name == "archived_sessions")
            .then(|| ancestor.parent().map(Path::to_path_buf))
            .flatten()
    }) else {
        return ReplayResolution::Deferred;
    };
    let mut matching_parents = Vec::new();
    let mut found_parent = false;
    let has_embedded_parent = session_identities_from_lines(child_lines)
        .iter()
        .any(|parent| parent.thread_id == parent_id);
    let fallback_catalog;
    let replay_catalog = match replay_catalog {
        Some(catalog) => catalog,
        None => {
            fallback_catalog = ReplayParentCatalog::new(collect_codex_session_files(&codex_dir));
            &fallback_catalog
        }
    };
    for candidate in replay_catalog.candidates_for(parent_id) {
        if candidate == child_path {
            continue;
        }
        let Some(timeline) = replay_catalog.timeline_for(&candidate) else {
            continue;
        };
        found_parent = true;
        if let Some(signatures) = parent_signatures_before_timeline(&timeline, cutoff) {
            matching_parents.push(signatures);
        }
    }
    if matching_parents.is_empty() && has_embedded_parent {
        found_parent = true;
        if let Some(signatures) = embedded_parent_signatures(child_lines, identity, cutoff) {
            matching_parents.push(signatures);
        }
    }
    if !found_parent || matching_parents.is_empty() {
        return ReplayResolution::Deferred;
    }
    let first = &matching_parents[0];
    if matching_parents
        .iter()
        .skip(1)
        .any(|candidate| candidate != first)
    {
        return ReplayResolution::SuspectedDuplicate;
    }
    ReplayResolution::Matched(matching_replay_prefix(child_events, first))
}

fn embedded_parent_signatures(
    lines: &[String],
    identity: &CodexSessionIdentity,
    cutoff: DateTime<Utc>,
) -> Option<Vec<CumulativeTokens>> {
    let boundary = history_replay_boundary(lines, Some(identity))?;
    Some(
        lines
            .iter()
            .take(boundary)
            .filter_map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|value| token_usage_signature(&value))
            })
            .collect::<Vec<CumulativeTokens>>(),
    )
    .filter(|signatures| !signatures.is_empty())
    .or_else(|| parent_signatures_before(lines, cutoff))
}

fn parent_signatures_before(
    lines: &[String],
    cutoff: DateTime<Utc>,
) -> Option<Vec<CumulativeTokens>> {
    let timeline = replay_parent_timeline_from_lines(lines);
    parent_signatures_before_timeline(&timeline, cutoff)
}

fn replay_parent_timeline_from_lines(lines: &[String]) -> ReplayParentTimeline {
    let mut timeline = ReplayParentTimeline::default();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        append_replay_parent_timeline_value(&mut timeline, &value);
    }
    timeline
}

fn append_replay_parent_timeline_value(timeline: &mut ReplayParentTimeline, value: &Value) {
    let Some(timestamp) = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
    else {
        return;
    };
    timeline.max_timestamp = Some(
        timeline
            .max_timestamp
            .map_or(timestamp, |current| current.max(timestamp)),
    );
    if is_session_terminal_event(value) {
        timeline.terminal_timestamps.push(timestamp);
    }
    if let Some(signature) = token_usage_signature(value) {
        timeline.signatures.push((timestamp, signature));
    }
    if let Some(model) = value
        .get("payload")
        .and_then(|payload| {
            pick_string(
                payload,
                &[
                    &["model"],
                    &["model_name"],
                    &["modelName"],
                    &["info", "model"],
                ],
            )
        })
        .map(|model| normalize_codex_model(&model))
        .filter(|model| model != "unknown")
    {
        timeline.models.push((timestamp, model));
    }
}

fn parent_signatures_before_timeline(
    timeline: &ReplayParentTimeline,
    cutoff: DateTime<Utc>,
) -> Option<Vec<CumulativeTokens>> {
    let covers_cutoff = timeline
        .max_timestamp
        .is_some_and(|timestamp| timestamp >= cutoff)
        || timeline
            .terminal_timestamps
            .iter()
            .any(|timestamp| *timestamp <= cutoff);
    covers_cutoff.then(|| {
        timeline
            .signatures
            .iter()
            .filter(|(timestamp, _)| *timestamp <= cutoff)
            .map(|(_, signature)| signature.clone())
            .collect()
    })
}

fn parent_model_before_timeline(
    timeline: &ReplayParentTimeline,
    cutoff: DateTime<Utc>,
) -> Option<String> {
    timeline
        .models
        .iter()
        .filter(|(timestamp, _)| *timestamp <= cutoff)
        .max_by_key(|(timestamp, _)| *timestamp)
        .map(|(_, model)| model.clone())
}

fn matching_replay_prefix(child: &[ParsedTokenUsageEvent], parent: &[CumulativeTokens]) -> usize {
    let mut parent_offset = 0;
    let mut matched = 0;
    for event in child {
        let Some(relative_match) = parent[parent_offset..]
            .iter()
            .position(|signature| signature == &event.signature)
        else {
            break;
        };
        parent_offset += relative_match + 1;
        matched += 1;
    }
    matched
}

fn history_replay_boundary(
    lines: &[String],
    identity: Option<&CodexSessionIdentity>,
) -> Option<usize> {
    if !identity.is_some_and(|identity| identity.carries_history_snapshot) {
        return None;
    }
    lines.iter().enumerate().find_map(|(index, line)| {
        if !line.contains("thread_settings_applied") && !line.contains("inter_agent_communication")
        {
            return None;
        }
        let value = serde_json::from_str::<Value>(line).ok()?;
        let event_type = value.get("type").and_then(Value::as_str)?;
        let is_boundary = event_type.starts_with("inter_agent_communication")
            || (event_type == "event_msg"
                && value.pointer("/payload/type").and_then(Value::as_str)
                    == Some("thread_settings_applied"));
        is_boundary.then_some(index + 1)
    })
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
    let temporary_path = path.with_extension("tmp");
    fs::write(&temporary_path, text)
        .map_err(|source| CompanionError::io(&temporary_path, source))?;
    if let Err(source) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(CompanionError::io(path, source));
    }
    Ok(())
}

fn apply_session_meta(state: &mut FileParseState, value: &Value) {
    let Some(payload) = value.get("payload") else {
        return;
    };
    if state.session_id.is_none() {
        state.session_id = pick_string(
            payload,
            &[
                &["id"],
                &["thread_id"],
                &["threadId"],
                &["session_id"],
                &["sessionId"],
            ],
        );
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
    let cached_input = delta.cached_input.min(delta.input);
    let delta = DeltaTokens {
        cached_input,
        cache_write_input: delta
            .cache_write_input
            .min(delta.input.saturating_sub(cached_input)),
        ..delta
    };
    if delta.is_zero() {
        return None;
    }

    Some(TokenUsageEvent {
        event_id: None,
        timestamp: pick_string(value, &[&["timestamp"]]),
        session_id: state.session_id.clone(),
        model: state.current_model.clone(),
        provider_id: state.current_provider_id.clone(),
        input_tokens: delta.fresh_input(),
        cached_input_tokens: delta.cached_input,
        cache_write_input_tokens: delta.cache_write_input,
        output_tokens: delta.output,
        total_tokens: delta.total(),
        cost: None,
        pricing_model: None,
        pricing_source: None,
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
            cache_write_input: last.cache_write_input,
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
        cache_write_input: pick_u64(
            value,
            &[
                &["cache_creation_input_tokens"],
                &["cacheCreationInputTokens"],
                &["cache_write_input_tokens"],
                &["cacheWriteInputTokens"],
                &["cached_write_input_tokens"],
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
            cache_write_input: current.cache_write_input,
            output: current.output,
        },
        Some(prev) => DeltaTokens {
            input: current.input.saturating_sub(prev.input),
            cached_input: current.cached_input.saturating_sub(prev.cached_input),
            cache_write_input: current
                .cache_write_input
                .saturating_sub(prev.cache_write_input),
            output: current.output.saturating_sub(prev.output),
        },
    }
}

pub(crate) fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
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
    accumulator.bucket.cache_write_input_tokens += event.cache_write_input_tokens;
    accumulator.bucket.output_tokens += event.output_tokens;
    accumulator.bucket.total_tokens += event.total_tokens;
    if let Some(cost) = cost {
        accumulator.bucket.priced_events += 1;
        if event.pricing_source == Some(TokenUsagePricingSource::InferredParentModel) {
            accumulator.bucket.inferred_priced_events += 1;
        }
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

fn token_event_key(event: &TokenUsageEvent) -> String {
    event
        .event_id
        .clone()
        .unwrap_or_else(|| token_event_fingerprint(event))
}

fn stable_event_id(event: &TokenUsageEvent, line_index: usize) -> String {
    let source = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        event.session_id.as_deref().unwrap_or(""),
        event.timestamp.as_deref().unwrap_or(""),
        event.provider_id.as_deref().unwrap_or(""),
        event.model,
        event.input_tokens,
        event.cached_input_tokens,
        event.cache_write_input_tokens,
        event.output_tokens,
        line_index,
    );
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

fn token_event_fingerprint(event: &TokenUsageEvent) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        event.session_id.as_deref().unwrap_or(""),
        event.timestamp.as_deref().unwrap_or(""),
        event.provider_id.as_deref().unwrap_or(""),
        event.model,
        event.input_tokens,
        event.cached_input_tokens,
        event.cache_write_input_tokens,
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
    use chrono::TimeZone;
    use std::io::Write;

    #[test]
    fn validates_and_applies_inclusive_date_ranges() {
        let range = TokenUsageDateRange::parse(Some("2026-07-10"), Some("2026-07-12"))
            .expect("valid range");
        assert!(range.includes(Some(&local_timestamp(2026, 7, 10))));
        assert!(range.includes(Some(&local_timestamp(2026, 7, 12))));
        assert!(!range.includes(Some(&local_timestamp(2026, 7, 9))));
        assert!(!range.includes(Some(&local_timestamp(2026, 7, 13))));
        assert!(!range.includes(None));
        assert!(TokenUsageDateRange::parse(Some("2026-07-12"), Some("2026-07-10")).is_err());
        assert!(TokenUsageDateRange::parse(Some("07/10/2026"), None).is_err());
    }

    #[test]
    fn filters_with_local_time_boundaries_to_the_second() {
        let range =
            TokenUsageDateRange::parse(Some("2026-07-10T09:30:15"), Some("2026-07-10T17:45:20"))
                .expect("valid time range");

        assert!(!range.includes(Some(&local_timestamp_at(2026, 7, 10, 9, 30, 14))));
        assert!(range.includes(Some(&local_timestamp_at(2026, 7, 10, 9, 30, 15))));
        assert!(range.includes(Some(&local_timestamp_at(2026, 7, 10, 17, 45, 20))));
        assert!(!range.includes(Some(&local_timestamp_at(2026, 7, 10, 17, 45, 21))));
        assert!(TokenUsageDateRange::parse(
            Some("2026-07-10T17:45:20"),
            Some("2026-07-10T09:30:15")
        )
        .is_err());
    }

    #[test]
    fn date_range_filters_every_summary_dimension() {
        let range = TokenUsageDateRange::parse(Some("2026-07-10"), Some("2026-07-12"))
            .expect("valid range");
        let events = vec![
            usage_event("before", "provider-before", 2026, 7, 9, 100),
            usage_event("inside-a", "provider-a", 2026, 7, 10, 200),
            usage_event("inside-b", "provider-b", 2026, 7, 12, 300),
            usage_event("after", "provider-after", 2026, 7, 13, 400),
        ];
        let summary = summarize_token_events(
            TokenUsageSummaryInput {
                codex_dir: PathBuf::from("/tmp/codex"),
                files_scanned: 4,
                deferred_files: 0,
                suspected_duplicates: 0,
                events,
            },
            &PricingCatalog::builtin(),
            &range,
            &TokenUsageFilters::default(),
        );

        assert_eq!(summary.files_scanned, 4);
        assert_eq!(summary.sessions, 2);
        assert_eq!(summary.events, 2);
        assert_eq!(summary.total_tokens, 500);
        assert_eq!(summary.by_day.len(), 2);
        assert_eq!(summary.by_model.len(), 2);
        assert_eq!(summary.by_provider.len(), 2);
        assert_eq!(summary.recent_events.len(), 2);
        assert!(summary
            .by_provider
            .iter()
            .all(|bucket| bucket.key == "provider-a" || bucket.key == "provider-b"));
    }

    #[test]
    fn provider_and_model_filters_apply_to_every_summary_dimension() {
        let events = vec![
            usage_event("model-a", "provider-a", 2026, 7, 10, 100),
            usage_event("model-b", "provider-a", 2026, 7, 11, 200),
            usage_event("model-a", "provider-b", 2026, 7, 12, 300),
        ];
        let filters = TokenUsageFilters::parse(Some("provider-a"), Some("model-b"));
        let summary = summarize_token_events(
            TokenUsageSummaryInput {
                codex_dir: PathBuf::from("/tmp/codex"),
                files_scanned: 3,
                deferred_files: 0,
                suspected_duplicates: 0,
                events,
            },
            &PricingCatalog::builtin(),
            &TokenUsageDateRange::default(),
            &filters,
        );

        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.events, 1);
        assert_eq!(summary.total_tokens, 200);
        assert_eq!(summary.by_day.len(), 1);
        assert_eq!(summary.by_model.len(), 1);
        assert_eq!(summary.by_model[0].key, "model-b");
        assert_eq!(summary.by_provider.len(), 1);
        assert_eq!(summary.by_provider[0].key, "provider-a");
        assert_eq!(summary.recent_events.len(), 1);
        assert_eq!(
            summary.available_providers,
            vec!["provider-a", "provider-b"]
        );
        assert_eq!(summary.available_models, vec!["model-a", "model-b"]);
    }

    #[test]
    fn stable_event_ids_keep_distinct_same_second_usage() {
        let mut first = usage_event("session", "provider", 2026, 7, 10, 100);
        let mut second = first.clone();
        first.event_id = Some(stable_event_id(&first, 10));
        second.event_id = Some(stable_event_id(&second, 11));
        let summary = summarize_token_events(
            TokenUsageSummaryInput {
                codex_dir: PathBuf::from("/tmp/codex"),
                files_scanned: 1,
                deferred_files: 0,
                suspected_duplicates: 0,
                events: vec![first, second],
            },
            &PricingCatalog::builtin(),
            &TokenUsageDateRange::default(),
            &TokenUsageFilters::default(),
        );

        assert_eq!(summary.events, 2);
        assert_eq!(summary.total_tokens, 200);
    }

    fn local_timestamp(year: i32, month: u32, day: u32) -> String {
        local_timestamp_at(year, month, day, 12, 0, 0)
    }

    fn local_timestamp_at(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> String {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .expect("local timestamp")
            .to_rfc3339()
    }

    fn usage_event(
        session_id: &str,
        provider_id: &str,
        year: i32,
        month: u32,
        day: u32,
        total_tokens: u64,
    ) -> TokenUsageEvent {
        TokenUsageEvent {
            timestamp: Some(local_timestamp(year, month, day)),
            session_id: Some(session_id.to_string()),
            model: session_id.to_string(),
            provider_id: Some(provider_id.to_string()),
            input_tokens: total_tokens,
            total_tokens,
            ..TokenUsageEvent::default()
        }
    }

    #[test]
    fn computes_delta_from_cumulative_token_count() {
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 40,
            cache_write_input: 10,
            output: 10,
        });
        let current = CumulativeTokens {
            input: 160,
            cached_input: 80,
            cache_write_input: 15,
            output: 30,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 60);
        assert_eq!(delta.cached_input, 40);
        assert_eq!(delta.cache_write_input, 5);
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
    fn auto_review_uses_the_parent_model_for_pricing() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("08")
            .join("08");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("parent.jsonl"),
            r#"{"timestamp":"2026-08-08T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:00.500Z","type":"turn_context","payload":{"model":"gpt-5.4"}}"#
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:03Z","type":"event_msg","payload":{"type":"task_complete"}}"#
                + "\n",
        )
        .expect("parent");
        fs::write(
            day.join("review.jsonl"),
            r#"{"timestamp":"2026-08-08T03:00:04Z","type":"session_meta","payload":{"id":"review","parent_thread_id":"parent","source":{"subagent":{"other":"guardian"}}}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:04.500Z","type":"turn_context","payload":{"model":"codex-auto-review"}}"#
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":150,"cached_input_tokens":100,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("review");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        let review_event = summary
            .recent_events
            .iter()
            .find(|event| event.session_id.as_deref() == Some("review"))
            .expect("review event");

        assert_eq!(summary.unpriced_events, 0);
        assert_eq!(summary.priced_events, 2);
        assert_eq!(summary.inferred_priced_events, 1);
        assert_eq!(review_event.model, CODEX_AUTO_REVIEW_MODEL);
        assert_eq!(review_event.pricing_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            review_event.pricing_source,
            Some(TokenUsagePricingSource::InferredParentModel)
        );
        let review_bucket = summary
            .by_model
            .iter()
            .find(|bucket| bucket.key == CODEX_AUTO_REVIEW_MODEL)
            .expect("auto-review bucket");
        assert_eq!(review_bucket.priced_events, 1);
        assert_eq!(review_bucket.inferred_priced_events, 1);
    }

    #[test]
    fn auto_review_stays_unpriced_when_parent_model_is_ambiguous() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("08")
            .join("08");
        fs::create_dir_all(&day).expect("mkdir");
        for (name, model) in [
            ("parent-a.jsonl", "gpt-5.4"),
            ("parent-b.jsonl", "gpt-5.6-luna"),
        ] {
            fs::write(
                day.join(name),
                format!(
                    concat!(
                        "{{\"timestamp\":\"2026-08-08T03:00:00Z\",\"type\":\"session_meta\",",
                        "\"payload\":{{\"id\":\"parent\"}}}}\n",
                        "{{\"timestamp\":\"2026-08-08T03:00:00.500Z\",\"type\":\"turn_context\",",
                        "\"payload\":{{\"model\":\"{}\"}}}}\n",
                        "{{\"timestamp\":\"2026-08-08T03:00:01Z\",\"type\":\"event_msg\",",
                        "\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":",
                        "{{\"input_tokens\":100,\"output_tokens\":10}}}}}}}}\n",
                        "{{\"timestamp\":\"2026-08-08T03:00:03Z\",\"type\":\"event_msg\",",
                        "\"payload\":{{\"type\":\"task_complete\"}}}}\n"
                    ),
                    model
                ),
            )
            .expect("parent");
        }
        fs::write(
            day.join("review.jsonl"),
            r#"{"timestamp":"2026-08-08T03:00:04Z","type":"session_meta","payload":{"id":"review","parent_thread_id":"parent","source":{"subagent":{"other":"guardian"}}}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:04.500Z","type":"turn_context","payload":{"model":"codex-auto-review"}}"#
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":150,"cached_input_tokens":100,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("review");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        let review_event = summary
            .recent_events
            .iter()
            .find(|event| event.session_id.as_deref() == Some("review"))
            .expect("review event");

        assert_eq!(summary.unpriced_events, 1);
        assert_eq!(summary.inferred_priced_events, 0);
        assert_eq!(summary.unpriced_models, vec![CODEX_AUTO_REVIEW_MODEL]);
        assert_eq!(review_event.pricing_model, None);
        assert_eq!(review_event.pricing_source, None);
        assert!(review_event.cost.is_none());
    }

    #[test]
    fn cached_auto_review_pricing_is_recomputed_when_parent_catalog_changes() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("08")
            .join("08");
        fs::create_dir_all(&day).expect("mkdir");
        let parent = |model: &str| {
            format!(
                concat!(
                    "{{\"timestamp\":\"2026-08-08T03:00:00Z\",\"type\":\"session_meta\",",
                    "\"payload\":{{\"id\":\"parent\"}}}}\n",
                    "{{\"timestamp\":\"2026-08-08T03:00:00.500Z\",\"type\":\"turn_context\",",
                    "\"payload\":{{\"model\":\"{}\"}}}}\n",
                    "{{\"timestamp\":\"2026-08-08T03:00:01Z\",\"type\":\"event_msg\",",
                    "\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":",
                    "{{\"input_tokens\":100,\"output_tokens\":10}}}}}}}}\n",
                    "{{\"timestamp\":\"2026-08-08T03:00:03Z\",\"type\":\"event_msg\",",
                    "\"payload\":{{\"type\":\"task_complete\"}}}}\n"
                ),
                model
            )
        };
        fs::write(day.join("parent-a.jsonl"), parent("gpt-5.4")).expect("parent a");
        fs::write(
            day.join("review.jsonl"),
            r#"{"timestamp":"2026-08-08T03:00:04Z","type":"session_meta","payload":{"id":"review","parent_thread_id":"parent","source":{"subagent":{"other":"guardian"}}}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:04.500Z","type":"turn_context","payload":{"model":"codex-auto-review"}}"#
                + "\n"
                + r#"{"timestamp":"2026-08-08T03:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":150,"cached_input_tokens":100,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("review");
        let cache_dir = temp.path().join("cache");

        let first = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("first scan");
        let first_review = first
            .recent_events
            .iter()
            .find(|event| event.session_id.as_deref() == Some("review"))
            .expect("first review event");
        assert_eq!(first.inferred_priced_events, 1);
        assert_eq!(first_review.pricing_model.as_deref(), Some("gpt-5.4"));
        let cache = read_token_usage_cache(&cache_dir.join("token-usage-cache.json"));
        let cached_review = cache
            .files
            .values()
            .flat_map(|file| &file.events)
            .find(|event| event.session_id.as_deref() == Some("review"))
            .expect("cached review event");
        assert_eq!(cached_review.pricing_model, None);
        assert_eq!(cached_review.pricing_source, None);

        fs::write(day.join("parent-b.jsonl"), parent("gpt-5.6-luna")).expect("parent b");
        let second =
            collect_token_usage_cached(temp.path().to_path_buf(), cache_dir).expect("second scan");
        let second_review = second
            .recent_events
            .iter()
            .find(|event| event.session_id.as_deref() == Some("review"))
            .expect("second review event");

        assert_eq!(second.inferred_priced_events, 0);
        assert_eq!(second_review.pricing_model, None);
        assert_eq!(second_review.pricing_source, None);
        assert!(second_review.cost.is_none());
    }

    #[test]
    fn conflicting_explicit_parent_ids_do_not_fall_back_to_legacy_parent() {
        let identity = parse_codex_session_identity(
            &serde_json::json!({
                "id": "review",
                "session_id": "legacy-parent",
                "forked_from_id": "parent-a",
                "parent_thread_id": "parent-b"
            }),
            Some("2026-08-08T03:00:04Z"),
        )
        .expect("identity");

        assert_eq!(identity.parent_thread_id, None);
        assert!(identity.carries_history_snapshot);
    }

    #[test]
    fn replay_catalog_indexes_metadata_and_caches_only_compact_timelines() {
        let temp = tempfile::tempdir().expect("temp");
        let parent_path = temp.path().join("parent.jsonl");
        let child_path = temp.path().join("child.jsonl");
        fs::write(
            &parent_path,
            r#"{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"task_complete"}}"#
                + "\n",
        )
        .expect("parent");
        fs::write(
            &child_path,
            r#"{"timestamp":"2026-07-10T03:00:03Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#
                + "\n",
        )
        .expect("child");

        let catalog = ReplayParentCatalog::new(vec![child_path, parent_path.clone()]);

        assert_eq!(catalog.candidates_for("parent"), vec![parent_path.clone()]);
        assert!(catalog.timelines.lock().expect("timelines").is_empty());
        let timeline = catalog.timeline_for(&parent_path).expect("timeline");
        assert_eq!(timeline.signatures.len(), 1);
        assert_eq!(timeline.terminal_timestamps.len(), 1);
        assert_eq!(catalog.timelines.lock().expect("timelines").len(), 1);
    }

    #[test]
    fn subagent_history_replay_only_establishes_cumulative_baseline() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("child.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"child","session_id":"parent","source":{"subagent":{}}}}"#.to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":900,"output_tokens":100}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200,"cached_input_tokens":1000,"output_tokens":120}}}}"#
                + "\n"
                + r#"{"type":"event_msg","payload":{"type":"thread_settings_applied"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1300,"cached_input_tokens":1050,"output_tokens":150}}}}"#
                + "\n",
        )
        .expect("write");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.events, 1);
        assert_eq!(
            summary.recent_events[0].session_id.as_deref(),
            Some("child")
        );
        assert_eq!(summary.input_tokens, 50);
        assert_eq!(summary.cached_input_tokens, 50);
        assert_eq!(summary.output_tokens, 30);
        assert_eq!(summary.total_tokens, 130);
    }

    #[test]
    fn fork_replay_uses_parent_token_prefix_alignment() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("parent.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"output_tokens":20}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:05Z","type":"event_msg","payload":{"type":"task_complete"}}"#
                + "\n",
        )
        .expect("parent");
        fs::write(
            day.join("child.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"output_tokens":20}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":250,"output_tokens":25}}}}"#
                + "\n",
        )
        .expect("child");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.events, 3);
        assert_eq!(summary.sessions, 2);
        assert_eq!(summary.total_tokens, 275);
        assert_eq!(
            summary
                .recent_events
                .iter()
                .filter(|event| event.session_id.as_deref() == Some("child"))
                .count(),
            1
        );
    }

    #[test]
    fn completed_parent_before_fork_is_accepted() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("parent.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:03Z","type":"event_msg","payload":{"type":"task_complete"}}"#
                + "\n",
        )
        .expect("parent");
        fs::write(
            day.join("child.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("child");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.deferred_files, 0);
        assert_eq!(summary.suspected_duplicates, 0);
        assert_eq!(summary.events, 2);
        assert_eq!(summary.total_tokens, 165);
        assert_eq!(
            summary
                .recent_events
                .iter()
                .filter(|event| event.session_id.as_deref() == Some("child"))
                .count(),
            1
        );
    }

    #[test]
    fn embedded_parent_history_resolves_without_a_separate_parent_file() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("child.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"parent"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"type":"event_msg","payload":{"type":"thread_settings_applied"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("child");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.deferred_files, 0);
        assert_eq!(summary.suspected_duplicates, 0);
        assert_eq!(summary.events, 1);
        assert_eq!(summary.total_tokens, 55);
    }

    #[test]
    fn missing_parent_defers_child_until_a_later_cached_scan_can_resolve_it() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("child.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("child");
        let cache_dir = temp.path().join("cache");

        let deferred = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("deferred scan");
        assert_eq!(deferred.deferred_files, 1);
        assert_eq!(deferred.events, 0);

        fs::write(
            day.join("parent.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:00Z","type":"session_meta","payload":{"id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:05Z","type":"event_msg","payload":{"type":"task_complete"}}"#
                + "\n",
        )
        .expect("parent");

        let resolved = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir)
            .expect("resolved scan");
        assert_eq!(resolved.deferred_files, 0);
        assert_eq!(resolved.suspected_duplicates, 0);
        assert_eq!(
            resolved
                .recent_events
                .iter()
                .filter(|event| event.session_id.as_deref() == Some("child"))
                .count(),
            1
        );
    }

    #[test]
    fn conflicting_parent_histories_mark_the_child_as_a_suspected_duplicate() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        for (name, input_tokens) in [("parent-a.jsonl", 100), ("parent-b.jsonl", 120)] {
            fs::write(
                day.join(name),
                format!(
                    concat!(
                        "{{\"timestamp\":\"2026-07-10T03:00:00Z\",\"type\":\"session_meta\",",
                        "\"payload\":{{\"id\":\"parent\"}}}}\n",
                        "{{\"timestamp\":\"2026-07-10T03:00:01Z\",\"type\":\"event_msg\",",
                        "\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":",
                        "{{\"input_tokens\":{},\"output_tokens\":10}}}}}}}}\n",
                        "{{\"timestamp\":\"2026-07-10T03:00:05Z\",\"type\":\"event_msg\",",
                        "\"payload\":{{\"type\":\"task_complete\"}}}}\n"
                    ),
                    input_tokens
                ),
            )
            .expect("parent");
        }
        fs::write(
            day.join("child.jsonl"),
            r#"{"timestamp":"2026-07-10T03:00:04Z","type":"session_meta","payload":{"id":"child","forked_from_id":"parent"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"output_tokens":15}}}}"#
                + "\n",
        )
        .expect("child");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");

        assert_eq!(summary.deferred_files, 0);
        assert_eq!(summary.suspected_duplicates, 1);
        assert!(summary
            .recent_events
            .iter()
            .all(|event| event.session_id.as_deref() != Some("child")));
    }

    #[test]
    fn separates_cache_write_tokens_from_fresh_input() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("cache-write.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"cache-write"}}"#.to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"gpt-5.6-luna"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("write");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");
        assert_eq!(summary.input_tokens, 50);
        assert_eq!(summary.cached_input_tokens, 20);
        assert_eq!(summary.cache_write_input_tokens, 30);
        assert_eq!(summary.total_tokens, 110);
        assert_eq!(summary.cost.cache_write_input_usd, "0.0000075");
    }

    #[test]
    fn cached_scan_applies_manual_pricing_override() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        let override_path = temp.path().join("model-pricing.json");
        fs::write(
            &override_path,
            r#"{
              "models": [{
                "model": "gpt-5.6-terra",
                "inputPerMillion": "9",
                "cachedInputPerMillion": "0.9",
                "cacheWriteInputPerMillion": "1.1",
                "outputPerMillion": "18"
              }]
            }"#,
        )
        .expect("pricing file");
        fs::write(
            day.join("manual-pricing.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"manual-pricing"}}"#.to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("session");

        let summary =
            collect_token_usage_cached(temp.path().to_path_buf(), temp.path().join("cache"))
                .expect("summary");

        assert_eq!(
            summary.pricing_override_path.as_deref(),
            Some(override_path.as_path())
        );
        assert_eq!(summary.priced_events, 1);
        assert_eq!(summary.cost.fresh_input_usd, "0.00045");
        assert_eq!(summary.cost.cached_input_usd, "0.000018");
        assert_eq!(summary.cost.cache_write_input_usd, "0.000033");
        assert_eq!(summary.cost.output_usd, "0.00018");
        assert_eq!(summary.cost.total_usd, "0.000681");
    }

    #[test]
    fn non_cached_scan_applies_manual_pricing_override() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        let override_path = temp.path().join("model-pricing.json");
        fs::write(
            &override_path,
            r#"{
              "models": [{
                "model": "gpt-5.6-terra",
                "inputPerMillion": "9",
                "cachedInputPerMillion": "0.9",
                "cacheWriteInputPerMillion": "1.1",
                "outputPerMillion": "18"
              }]
            }"#,
        )
        .expect("pricing file");
        fs::write(
            day.join("manual-pricing.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"manual-pricing"}}"#.to_string()
                + "\n"
                + r#"{"type":"turn_context","payload":{"model":"gpt-5.6-terra"}}"#
                + "\n"
                + r#"{"timestamp":"2026-07-10T03:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("session");

        let summary = collect_token_usage(temp.path().to_path_buf()).expect("summary");

        assert_eq!(
            summary.pricing_override_path.as_deref(),
            Some(override_path.as_path())
        );
        assert_eq!(summary.priced_events, 1);
        assert_eq!(summary.cost.fresh_input_usd, "0.00045");
        assert_eq!(summary.cost.cached_input_usd, "0.000018");
        assert_eq!(summary.cost.cache_write_input_usd, "0.000033");
        assert_eq!(summary.cost.output_usd, "0.00018");
        assert_eq!(summary.cost.total_usd, "0.000681");
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
        let cache_path = cache_dir.join("token-usage-cache.json");
        let cache_modified = fs::metadata(&cache_path)
            .expect("cache metadata")
            .modified()
            .expect("cache modified");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("second");
        assert_eq!(first.total_tokens, 110);
        assert_eq!(second.total_tokens, first.total_tokens);
        assert_eq!(
            fs::metadata(&cache_path)
                .expect("unchanged cache metadata")
                .modified()
                .expect("unchanged cache modified"),
            cache_modified
        );

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
    fn cached_scan_replays_only_appended_cumulative_usage() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("08");
        fs::create_dir_all(&day).expect("mkdir");
        let session = day.join("session.jsonl");
        fs::write(
            &session,
            r#"{"type":"session_meta","payload":{"id":"s1"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-06-08T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("write");
        let cache_dir = temp.path().join("cache");
        let first = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("first scan");
        assert_eq!(first.total_tokens, 110);

        let appended = r#"{"timestamp":"2026-06-08T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":15}}}}"#
            .to_string()
            + "\n";
        let mut handle = fs::OpenOptions::new()
            .append(true)
            .open(&session)
            .expect("open");
        handle.write_all(appended.as_bytes()).expect("append");

        let second = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("second scan");
        assert_eq!(second.events, 2);
        assert_eq!(second.total_tokens, 145);
        let cache = read_token_usage_cache(&cache_dir.join("token-usage-cache.json"));
        let cached = cache.files.values().next().expect("cached file");
        assert_eq!(cached.parsed_lines, 3);
        assert_eq!(
            cached.parsed_bytes,
            fs::metadata(&session).expect("metadata").len()
        );
    }

    #[test]
    fn incomplete_appended_line_is_not_counted_until_terminated() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("09");
        fs::create_dir_all(&day).expect("mkdir");
        let session = day.join("session.jsonl");
        fs::write(
            &session,
            r#"{"type":"session_meta","payload":{"id":"s1"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-06-09T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("write");
        let cache_dir = temp.path().join("cache");
        let first = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("first scan");
        assert_eq!(first.total_tokens, 110);

        let complete_line = r#"{"timestamp":"2026-06-09T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":20,"output_tokens":5}}}}"#;
        let split_at = complete_line.len() / 2;
        let mut handle = fs::OpenOptions::new()
            .append(true)
            .open(&session)
            .expect("open");
        handle
            .write_all(&complete_line.as_bytes()[..split_at])
            .expect("partial append");
        drop(handle);

        let partial = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("partial scan");
        assert_eq!(partial.events, 1);
        assert_eq!(partial.total_tokens, 110);

        let mut handle = fs::OpenOptions::new()
            .append(true)
            .open(&session)
            .expect("open");
        handle
            .write_all(&complete_line.as_bytes()[split_at..])
            .expect("finish append");
        handle.write_all(b"\n").expect("newline");

        let complete = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir)
            .expect("complete scan");
        assert_eq!(complete.events, 2);
        assert_eq!(complete.total_tokens, 135);
    }

    #[test]
    fn changed_file_prefix_falls_back_to_a_full_parse() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("06")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        let session = day.join("session.jsonl");
        let token_line = r#"{"timestamp":"2026-06-10T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#;
        fs::write(
            &session,
            r#"{"type":"session_meta","payload":{"id":"s1"}}"#.to_string()
                + "\n"
                + token_line
                + "\n",
        )
        .expect("write");
        let cache_dir = temp.path().join("cache");
        collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone()).expect("first");

        fs::write(
            &session,
            r#"{"type":"session_meta","payload":{"id":"s2"}}"#.to_string()
                + "\n"
                + token_line
                + "\n",
        )
        .expect("rewrite");
        let rebuilt = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir)
            .expect("rewritten scan");
        assert_eq!(rebuilt.total_tokens, 110);
        assert_eq!(rebuilt.sessions, 1);
        assert_eq!(rebuilt.recent_events[0].session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn old_cache_version_is_discarded_and_rebuilt() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("session.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"s1"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("session");
        let cache_dir = temp.path().join("cache");
        let first = collect_token_usage_cached(temp.path().to_path_buf(), cache_dir.clone())
            .expect("first scan");
        assert_eq!(first.total_tokens, 110);

        let cache_path = cache_dir.join("token-usage-cache.json");
        let mut cache = read_token_usage_cache(&cache_path);
        cache.version = TOKEN_USAGE_CACHE_VERSION - 1;
        for cached_file in cache.files.values_mut() {
            cached_file.events.clear();
        }
        write_token_usage_cache(&cache_path, &cache).expect("old cache");

        let rebuilt =
            collect_token_usage_cached(temp.path().to_path_buf(), cache_dir).expect("rebuilt scan");
        assert_eq!(rebuilt.total_tokens, 110);
        assert_eq!(rebuilt.events, 1);
        assert_eq!(rebuilt.cache_version, TOKEN_USAGE_CACHE_VERSION);
    }

    #[test]
    fn rebuild_discards_cached_events_and_rescans_session_files() {
        let temp = tempfile::tempdir().expect("temp");
        let day = temp
            .path()
            .join("sessions")
            .join("2026")
            .join("07")
            .join("10");
        fs::create_dir_all(&day).expect("mkdir");
        fs::write(
            day.join("session.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"s1"}}"#.to_string()
                + "\n"
                + r#"{"timestamp":"2026-07-10T01:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10}}}}"#
                + "\n",
        )
        .expect("write");
        let cache_dir = temp.path().join("cache");
        let range = TokenUsageDateRange::default();
        let filters = TokenUsageFilters::default();
        let first = collect_token_usage_cached_with_filters(
            temp.path().to_path_buf(),
            cache_dir.clone(),
            &range,
            &filters,
        )
        .expect("initial scan");
        assert_eq!(first.total_tokens, 110);

        let cache_path = cache_dir.join("token-usage-cache.json");
        let mut cache = read_token_usage_cache(&cache_path);
        for cached_file in cache.files.values_mut() {
            cached_file.events.clear();
        }
        write_token_usage_cache(&cache_path, &cache).expect("poison cache");

        let rebuilt = rebuild_token_usage_cached_with_filters(
            temp.path().to_path_buf(),
            cache_dir,
            &range,
            &filters,
        )
        .expect("rebuild");
        assert_eq!(rebuilt.events, 1);
        assert_eq!(rebuilt.total_tokens, 110);
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
