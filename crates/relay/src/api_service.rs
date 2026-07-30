use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, SecondsFormat, Utc};
use codex_companion_core::{
    redact_sensitive_text, ApiClient, ApiClientCreate, ApiClientHealth, ApiClientPeriodUsage,
    ApiClientSecret, ApiClientUpdate, ApiClientUsage, ApiRequestAttemptLog, ApiRequestLog,
    ApiServiceSnapshot, CompanionError, ConfigStore, ModelCooldown, Result,
};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;
use subtle::ConstantTimeEq;

const MAX_CLIENT_NAME_LEN: usize = 80;
const MAX_ALLOWED_MODELS: usize = 100;
const MAX_MODEL_NAME_LEN: usize = 160;
const API_KEY_PREFIX_LEN: usize = 16;
const MAX_LOG_ERROR_LEN: usize = 800;

#[derive(Debug, Clone)]
pub struct ApiServiceStore {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RequestLogStart<'a> {
    pub request_id: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub model: Option<&'a str>,
    pub client_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct RequestLogFinish<'a> {
    pub request_id: &'a str,
    pub provider_id: Option<&'a str>,
    pub status_code: Option<u16>,
    pub outcome: &'a str,
    pub attempts: u16,
    pub latency_ms: u64,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct RequestAttemptStart<'a> {
    pub request_id: &'a str,
    pub attempt: u16,
    pub provider_id: &'a str,
    pub route_reason: &'a str,
}

#[derive(Debug, Clone)]
pub struct RequestAttemptFinish<'a> {
    pub request_id: &'a str,
    pub attempt: u16,
    pub status_code: Option<u16>,
    pub outcome: &'a str,
    pub latency_ms: u64,
    pub error: Option<&'a str>,
}

impl ApiServiceStore {
    pub fn from_config_store(store: &ConfigStore) -> Self {
        Self {
            path: store.data_dir().join("relay").join("api-service.sqlite3"),
        }
    }

    pub fn initialize(&self) -> Result<()> {
        let connection = self.connection()?;
        connection
            .execute_batch(
                r#"
                PRAGMA journal_mode = WAL;
                PRAGMA foreign_keys = ON;
                CREATE TABLE IF NOT EXISTS api_clients (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    key_prefix TEXT NOT NULL UNIQUE,
                    key_hash BLOB NOT NULL,
                    allowed_models TEXT NOT NULL DEFAULT '[]',
                    enabled INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    last_used_at TEXT,
                    request_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS api_requests (
                    request_id TEXT PRIMARY KEY,
                    started_at TEXT NOT NULL,
                    method TEXT NOT NULL,
                    path TEXT NOT NULL,
                    model TEXT,
                    client_id TEXT,
                    provider_id TEXT,
                    status_code INTEGER,
                    outcome TEXT NOT NULL DEFAULT 'processing',
                    attempts INTEGER NOT NULL DEFAULT 0,
                    latency_ms INTEGER,
                    error TEXT,
                    FOREIGN KEY(client_id) REFERENCES api_clients(id) ON DELETE SET NULL
                );
                CREATE INDEX IF NOT EXISTS idx_api_requests_started_at
                    ON api_requests(started_at DESC);
                CREATE INDEX IF NOT EXISTS idx_api_requests_client_id
                    ON api_requests(client_id);
                CREATE TABLE IF NOT EXISTS api_request_attempts (
                    request_id TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    provider_id TEXT NOT NULL,
                    route_reason TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    finished_at TEXT,
                    status_code INTEGER,
                    outcome TEXT NOT NULL DEFAULT 'processing',
                    latency_ms INTEGER,
                    error TEXT,
                    PRIMARY KEY(request_id, attempt),
                    FOREIGN KEY(request_id) REFERENCES api_requests(request_id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_api_request_attempts_request_id
                    ON api_request_attempts(request_id, attempt);
                CREATE TABLE IF NOT EXISTS session_affinity (
                    affinity_key TEXT PRIMARY KEY,
                    provider_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_session_affinity_updated_at
                    ON session_affinity(updated_at DESC);
                CREATE TABLE IF NOT EXISTS chat_history (
                    provider_id TEXT NOT NULL,
                    response_id TEXT NOT NULL,
                    messages TEXT NOT NULL,
                    tool_context TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY(provider_id, response_id)
                );
                CREATE INDEX IF NOT EXISTS idx_chat_history_updated_at
                    ON chat_history(updated_at DESC);
                CREATE TABLE IF NOT EXISTS model_cooldowns (
                    provider_id TEXT NOT NULL,
                    model TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    cooldown_until TEXT NOT NULL,
                    PRIMARY KEY(provider_id, model)
                );
                CREATE INDEX IF NOT EXISTS idx_model_cooldowns_until
                    ON model_cooldowns(cooldown_until);
                "#,
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn create_client(&self, input: ApiClientCreate) -> Result<ApiClientSecret> {
        let name = validate_client_name(&input.name)?;
        let allowed_models = normalize_models(input.allowed_models)?;
        let api_key = generate_api_key();
        let key_prefix = key_prefix(&api_key);
        let key_hash = hash_key(&api_key);
        let created_at = Utc::now();
        let id = generate_client_id();
        let models_json = serde_json::to_string(&allowed_models)
            .map_err(|error| CompanionError::InvalidConfig(error.to_string()))?;
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO api_clients \
                 (id, name, key_prefix, key_hash, allowed_models, enabled, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
                params![
                    id,
                    name,
                    key_prefix,
                    key_hash.as_slice(),
                    models_json,
                    timestamp(created_at)
                ],
            )
            .map_err(database_error)?;
        let client = self
            .client_by_id(&id)?
            .ok_or_else(|| CompanionError::InvalidConfig("API client 创建后无法读取".into()))?;
        Ok(ApiClientSecret { client, api_key })
    }

    pub fn update_client(&self, input: ApiClientUpdate) -> Result<ApiClient> {
        let id = input.id.trim();
        if id.is_empty() {
            return Err(CompanionError::InvalidConfig(
                "API client id 不能为空".into(),
            ));
        }
        let name = validate_client_name(&input.name)?;
        let allowed_models = normalize_models(input.allowed_models)?;
        let models_json = serde_json::to_string(&allowed_models)
            .map_err(|error| CompanionError::InvalidConfig(error.to_string()))?;
        let connection = self.connection()?;
        let changed = connection
            .execute(
                "UPDATE api_clients SET name = ?2, allowed_models = ?3, enabled = ?4 WHERE id = ?1",
                params![id, name, models_json, input.enabled],
            )
            .map_err(database_error)?;
        if changed == 0 {
            return Err(CompanionError::InvalidConfig(format!(
                "unknown API client: {id}"
            )));
        }
        self.client_by_id(id)?.ok_or_else(|| {
            CompanionError::InvalidConfig(format!("API client 更新后无法读取: {id}"))
        })
    }

    pub fn rotate_client_key(&self, id: &str) -> Result<ApiClientSecret> {
        let id = id.trim();
        let api_key = generate_api_key();
        let prefix = key_prefix(&api_key);
        let hash = hash_key(&api_key);
        let connection = self.connection()?;
        let changed = connection
            .execute(
                "UPDATE api_clients SET key_prefix = ?2, key_hash = ?3 WHERE id = ?1",
                params![id, prefix, hash.as_slice()],
            )
            .map_err(database_error)?;
        if changed == 0 {
            return Err(CompanionError::InvalidConfig(format!(
                "unknown API client: {id}"
            )));
        }
        let client = self.client_by_id(id)?.ok_or_else(|| {
            CompanionError::InvalidConfig(format!("API client 轮换后无法读取: {id}"))
        })?;
        Ok(ApiClientSecret { client, api_key })
    }

    pub fn delete_client(&self, id: &str) -> Result<bool> {
        let connection = self.connection()?;
        connection
            .execute("DELETE FROM api_clients WHERE id = ?1", params![id.trim()])
            .map(|changed| changed > 0)
            .map_err(database_error)
    }

    pub fn list_clients(&self) -> Result<Vec<ApiClient>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, key_prefix, allowed_models, enabled, created_at, \
                 last_used_at, request_count FROM api_clients ORDER BY created_at DESC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], api_client_from_row)
            .map_err(database_error)?;
        let clients = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(database_error)?;
        clients
            .into_iter()
            .map(|client| self.enrich_client(client))
            .collect()
    }

    pub fn authenticate(&self, api_key: &str) -> Result<Option<ApiClient>> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Ok(None);
        }
        let prefix = key_prefix(api_key);
        let connection = self.connection()?;
        let candidate = connection
            .query_row(
                "SELECT id, key_hash FROM api_clients WHERE key_prefix = ?1 AND enabled = 1",
                params![prefix],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        let Some((id, stored_hash)) = candidate else {
            return Ok(None);
        };
        let candidate_hash = hash_key(api_key);
        if stored_hash.len() != candidate_hash.len()
            || !bool::from(stored_hash.as_slice().ct_eq(candidate_hash.as_slice()))
        {
            return Ok(None);
        }
        connection
            .execute(
                "UPDATE api_clients SET last_used_at = ?2, request_count = request_count + 1 WHERE id = ?1",
                params![id, timestamp(Utc::now())],
            )
            .map_err(database_error)?;
        self.client_by_id(&id)
    }

    pub fn snapshot(&self, request_limit: usize) -> Result<ApiServiceSnapshot> {
        self.remove_expired_model_cooldowns()?;
        let clients = self.list_clients()?;
        let model_cooldowns = self.list_model_cooldowns()?;
        Ok(ApiServiceSnapshot {
            clients,
            recent_requests: self.list_requests(request_limit)?,
            model_cooldowns,
            affinity_bindings: self.affinity_binding_count(86_400)?,
            pool_health: Default::default(),
        })
    }

    pub fn bind_affinity(&self, affinity_key: &str, provider_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO session_affinity (affinity_key, provider_id, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(affinity_key) DO UPDATE SET provider_id = excluded.provider_id, updated_at = excluded.updated_at",
                params![affinity_key, provider_id, timestamp(Utc::now())],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn preferred_affinity(
        &self,
        affinity_key: &str,
        ttl_seconds: u64,
    ) -> Result<Option<String>> {
        let cutoff = Utc::now()
            - Duration::seconds(
                i64::try_from(ttl_seconds)
                    .unwrap_or(i64::MAX)
                    .clamp(60, 86_400),
            );
        let connection = self.connection()?;
        let provider = connection
            .query_row(
                "SELECT provider_id FROM session_affinity WHERE affinity_key = ?1 AND updated_at >= ?2",
                params![affinity_key, timestamp(cutoff)],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        if provider.is_some() {
            connection
                .execute(
                    "UPDATE session_affinity SET updated_at = ?2 WHERE affinity_key = ?1",
                    params![affinity_key, timestamp(Utc::now())],
                )
                .map_err(database_error)?;
        }
        Ok(provider)
    }

    pub fn affinity_binding_count(&self, ttl_seconds: u64) -> Result<u64> {
        let cutoff = Utc::now()
            - Duration::seconds(
                i64::try_from(ttl_seconds)
                    .unwrap_or(i64::MAX)
                    .clamp(60, 86_400),
            );
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM session_affinity WHERE updated_at < ?1",
                params![timestamp(cutoff)],
            )
            .map_err(database_error)?;
        connection
            .query_row("SELECT COUNT(*) FROM session_affinity", [], |row| {
                row.get::<_, u64>(0)
            })
            .map_err(database_error)
    }

    pub fn load_chat_history(
        &self,
        provider_id: &str,
        response_id: &str,
    ) -> Result<Option<(Value, Value)>> {
        let connection = self.connection()?;
        let stored = connection
            .query_row(
                "SELECT messages, tool_context FROM chat_history WHERE provider_id = ?1 AND response_id = ?2",
                params![provider_id, response_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        stored
            .map(|(messages, tool_context)| {
                let messages = serde_json::from_str(&messages).map_err(|error| {
                    CompanionError::InvalidConfig(format!("chat history messages invalid: {error}"))
                })?;
                let tool_context = serde_json::from_str(&tool_context).map_err(|error| {
                    CompanionError::InvalidConfig(format!(
                        "chat history tool context invalid: {error}"
                    ))
                })?;
                Ok((messages, tool_context))
            })
            .transpose()
    }

    pub fn store_chat_history(
        &self,
        provider_id: &str,
        response_id: &str,
        messages: &Value,
        tool_context: &Value,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO chat_history (provider_id, response_id, messages, tool_context, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(provider_id, response_id) DO UPDATE SET \
                 messages = excluded.messages, tool_context = excluded.tool_context, updated_at = excluded.updated_at",
                params![
                    provider_id,
                    response_id,
                    messages.to_string(),
                    tool_context.to_string(),
                    timestamp(Utc::now())
                ],
            )
            .map_err(database_error)?;
        connection
            .execute(
                "DELETE FROM chat_history WHERE (provider_id, response_id) IN (\
                    SELECT provider_id, response_id FROM chat_history ORDER BY updated_at DESC LIMIT -1 OFFSET 512\
                 )",
                [],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn record_request_start(&self, input: RequestLogStart<'_>) -> Result<()> {
        let path = redact_sensitive_text(input.path);
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO api_requests \
                 (request_id, started_at, method, path, model, client_id, outcome) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'processing')",
                params![
                    input.request_id,
                    timestamp(Utc::now()),
                    input.method,
                    path,
                    input.model,
                    input.client_id
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn record_request_finish(&self, input: RequestLogFinish<'_>) -> Result<()> {
        let error = input.error.map(compact_log_error);
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE api_requests SET provider_id = ?2, status_code = ?3, outcome = ?4, \
                 attempts = ?5, latency_ms = ?6, error = ?7 WHERE request_id = ?1",
                params![
                    input.request_id,
                    input.provider_id,
                    input.status_code,
                    input.outcome,
                    input.attempts,
                    input.latency_ms,
                    error
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn record_request_attempt_start(&self, input: RequestAttemptStart<'_>) -> Result<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO api_request_attempts \
                 (request_id, attempt, provider_id, route_reason, started_at, outcome) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'processing')",
                params![
                    input.request_id,
                    input.attempt,
                    input.provider_id,
                    input.route_reason,
                    timestamp(Utc::now()),
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn record_request_attempt_finish(&self, input: RequestAttemptFinish<'_>) -> Result<()> {
        let error = input.error.map(compact_log_error);
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE api_request_attempts SET finished_at = ?3, status_code = ?4, \
                 outcome = ?5, latency_ms = ?6, error = ?7 \
                 WHERE request_id = ?1 AND attempt = ?2",
                params![
                    input.request_id,
                    input.attempt,
                    timestamp(Utc::now()),
                    input.status_code,
                    input.outcome,
                    input.latency_ms,
                    error,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn record_stream_outcome(
        &self,
        request_id: &str,
        outcome: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let error = error.map(compact_log_error);
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE api_requests SET outcome = ?2, error = ?3 WHERE request_id = ?1",
                params![request_id, outcome, error],
            )
            .map_err(database_error)?;
        connection
            .execute(
                "UPDATE api_request_attempts SET finished_at = ?3, outcome = ?2, error = ?4 \
                 WHERE request_id = ?1 AND attempt = (\
                    SELECT MAX(attempt) FROM api_request_attempts WHERE request_id = ?1\
                 )",
                params![request_id, outcome, timestamp(Utc::now()), error],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn list_requests(&self, limit: usize) -> Result<Vec<ApiRequestLog>> {
        let connection = self.connection()?;
        let limit = limit.clamp(1, 500) as i64;
        let mut statement = connection
            .prepare(
                "SELECT r.request_id, r.started_at, r.method, r.path, r.model, r.client_id, \
                 c.name, r.provider_id, r.status_code, r.outcome, r.attempts, r.latency_ms, r.error \
                 FROM api_requests r LEFT JOIN api_clients c ON c.id = r.client_id \
                 ORDER BY r.started_at DESC LIMIT ?1",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![limit], api_request_from_row)
            .map_err(database_error)?;
        let mut requests = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(database_error)?;
        for request in &mut requests {
            request.attempt_log = request_attempts(&connection, &request.request_id)?;
        }
        Ok(requests)
    }

    pub fn clear_request_logs(&self) -> Result<usize> {
        let connection = self.connection()?;
        connection
            .execute("DELETE FROM api_requests", [])
            .map_err(database_error)
    }

    pub fn prune_request_logs(&self, retention_days: u16) -> Result<usize> {
        let cutoff = Utc::now() - Duration::days(i64::from(retention_days.max(1)));
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM api_requests WHERE started_at < ?1",
                params![timestamp(cutoff)],
            )
            .map_err(database_error)
    }

    pub fn set_model_cooldown(
        &self,
        provider_id: &str,
        model: &str,
        reason: &str,
        seconds: u64,
    ) -> Result<()> {
        let until = Utc::now()
            + Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX).clamp(1, 86_400));
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO model_cooldowns (provider_id, model, reason, cooldown_until) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT(provider_id, model) DO UPDATE SET \
                 reason = excluded.reason, cooldown_until = excluded.cooldown_until",
                params![
                    provider_id,
                    model,
                    compact_log_error(reason),
                    timestamp(until)
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn model_cooldown_active(&self, provider_id: &str, model: &str) -> Result<bool> {
        let connection = self.connection()?;
        let until = connection
            .query_row(
                "SELECT cooldown_until FROM model_cooldowns WHERE provider_id = ?1 AND model = ?2",
                params![provider_id, model],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        let Some(until) = until else {
            return Ok(false);
        };
        if parse_timestamp(&until)? > Utc::now() {
            return Ok(true);
        }
        connection
            .execute(
                "DELETE FROM model_cooldowns WHERE provider_id = ?1 AND model = ?2",
                params![provider_id, model],
            )
            .map_err(database_error)?;
        Ok(false)
    }

    pub fn clear_model_cooldown(&self, provider_id: &str, model: &str) -> Result<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM model_cooldowns WHERE provider_id = ?1 AND model = ?2",
                params![provider_id, model],
            )
            .map_err(database_error)?;
        Ok(())
    }

    fn list_model_cooldowns(&self) -> Result<Vec<ModelCooldown>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT provider_id, model, reason, cooldown_until FROM model_cooldowns \
                 ORDER BY cooldown_until ASC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                let raw_until: String = row.get(3)?;
                Ok(ModelCooldown {
                    provider_id: row.get(0)?,
                    model: row.get(1)?,
                    reason: row.get(2)?,
                    cooldown_until: parse_timestamp_sql(3, &raw_until)?,
                })
            })
            .map_err(database_error)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(database_error)
    }

    fn remove_expired_model_cooldowns(&self) -> Result<usize> {
        let connection = self.connection()?;
        connection
            .execute(
                "DELETE FROM model_cooldowns WHERE cooldown_until <= ?1",
                params![timestamp(Utc::now())],
            )
            .map_err(database_error)
    }

    fn client_by_id(&self, id: &str) -> Result<Option<ApiClient>> {
        let connection = self.connection()?;
        let client = connection
            .query_row(
                "SELECT id, name, key_prefix, allowed_models, enabled, created_at, \
                 last_used_at, request_count FROM api_clients WHERE id = ?1",
                params![id],
                api_client_from_row,
            )
            .optional()
            .map_err(database_error)?;
        match client {
            Some(client) => self.enrich_client(client).map(Some),
            None => Ok(None),
        }
    }

    fn enrich_client(&self, client: ApiClient) -> Result<ApiClient> {
        let usage = ApiClientUsage {
            today: self.client_period_usage(&client.id, period_start(Period::Today))?,
            week: self.client_period_usage(&client.id, period_start(Period::Week))?,
            month: self.client_period_usage(&client.id, period_start(Period::Month))?,
        };
        let health = self.client_health(&client.id, client.enabled)?;
        Ok(ApiClient {
            usage,
            health,
            ..client
        })
    }

    fn client_period_usage(&self, client_id: &str, start: String) -> Result<ApiClientPeriodUsage> {
        let connection = self.connection()?;
        let (requests, succeeded, failed, average_latency_ms) = connection
            .query_row(
                "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN outcome IN ('succeeded', 'local') THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN outcome NOT IN ('succeeded', 'local', 'processing') THEN 1 ELSE 0 END), 0), \
                    AVG(latency_ms) FROM api_requests WHERE client_id = ?1 AND started_at >= ?2",
                params![client_id, start],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, Option<f64>>(3)?,
                    ))
                },
            )
            .map_err(database_error)?;
        Ok(ApiClientPeriodUsage {
            requests,
            succeeded,
            failed,
            success_rate: percentage(succeeded, requests),
            average_latency_ms: average_latency_ms.map(|value| value.max(0.0).round() as u64),
        })
    }

    fn client_health(&self, client_id: &str, enabled: bool) -> Result<ApiClientHealth> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT started_at, outcome FROM api_requests WHERE client_id = ?1 ORDER BY started_at DESC LIMIT 50",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![client_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        let mut last_request_at = None;
        let mut last_success_at = None;
        let mut last_failure_at = None;
        let mut consecutive_failures = 0;
        for row in rows {
            let (started_at, outcome) = row.map_err(database_error)?;
            let parsed = parse_timestamp(&started_at)?;
            if last_request_at.is_none() {
                last_request_at = Some(parsed);
            }
            if outcome == "succeeded" || outcome == "local" {
                if last_success_at.is_none() {
                    last_success_at = Some(parsed);
                }
                break;
            }
            if outcome != "processing" {
                if last_failure_at.is_none() {
                    last_failure_at = Some(parsed);
                }
                consecutive_failures += 1;
            }
        }
        let status = if !enabled {
            "disabled"
        } else if consecutive_failures >= 3 {
            "degraded"
        } else if last_request_at.is_some() {
            "healthy"
        } else {
            "idle"
        };
        Ok(ApiClientHealth {
            status: status.to_string(),
            last_request_at,
            last_success_at,
            last_failure_at,
            consecutive_failures,
        })
    }

    fn connection(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        let connection = Connection::open(&self.path).map_err(database_error)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(database_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(database_error)?;
        Ok(connection)
    }
}

fn api_client_from_row(row: &Row<'_>) -> rusqlite::Result<ApiClient> {
    let models_json: String = row.get(3)?;
    let created_at: String = row.get(5)?;
    let last_used_at: Option<String> = row.get(6)?;
    Ok(ApiClient {
        id: row.get(0)?,
        name: row.get(1)?,
        key_prefix: row.get(2)?,
        allowed_models: serde_json::from_str(&models_json).unwrap_or_default(),
        enabled: row.get(4)?,
        created_at: parse_timestamp_sql(5, &created_at)?,
        last_used_at: last_used_at
            .as_deref()
            .map(|value| parse_timestamp_sql(6, value))
            .transpose()?,
        request_count: row.get(7)?,
        usage: ApiClientUsage::default(),
        health: ApiClientHealth::default(),
    })
}

#[derive(Debug, Clone, Copy)]
enum Period {
    Today,
    Week,
    Month,
}

fn period_start(period: Period) -> String {
    let today = Local::now().date_naive();
    let date = match period {
        Period::Today => today,
        Period::Week => today - Duration::days(i64::from(today.weekday().num_days_from_monday())),
        Period::Month => today.with_day(1).unwrap_or(today),
    };
    let local = date
        .and_time(NaiveTime::MIN)
        .and_local_timezone(Local)
        .single()
        .unwrap_or_else(Local::now);
    timestamp(local.with_timezone(&Utc))
}

fn percentage(successful: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((successful.saturating_mul(100) / total).min(100)) as u8
}

fn api_request_from_row(row: &Row<'_>) -> rusqlite::Result<ApiRequestLog> {
    let started_at: String = row.get(1)?;
    Ok(ApiRequestLog {
        request_id: row.get(0)?,
        started_at: parse_timestamp_sql(1, &started_at)?,
        method: row.get(2)?,
        path: row.get(3)?,
        model: row.get(4)?,
        client_id: row.get(5)?,
        client_name: row.get(6)?,
        provider_id: row.get(7)?,
        status_code: row.get(8)?,
        outcome: row.get(9)?,
        attempts: row.get(10)?,
        latency_ms: row.get(11)?,
        error: row.get(12)?,
        attempt_log: Vec::new(),
    })
}

fn request_attempts(
    connection: &Connection,
    request_id: &str,
) -> Result<Vec<ApiRequestAttemptLog>> {
    let mut statement = connection
        .prepare(
            "SELECT attempt, provider_id, route_reason, started_at, finished_at, status_code, \
             outcome, latency_ms, error FROM api_request_attempts \
             WHERE request_id = ?1 ORDER BY attempt ASC",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![request_id], api_request_attempt_from_row)
        .map_err(database_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(database_error)
}

fn api_request_attempt_from_row(row: &Row<'_>) -> rusqlite::Result<ApiRequestAttemptLog> {
    let started_at: String = row.get(3)?;
    let finished_at: Option<String> = row.get(4)?;
    Ok(ApiRequestAttemptLog {
        attempt: row.get(0)?,
        provider_id: row.get(1)?,
        route_reason: row.get(2)?,
        started_at: parse_timestamp_sql(3, &started_at)?,
        finished_at: finished_at
            .as_deref()
            .map(|value| parse_timestamp_sql(4, value))
            .transpose()?,
        status_code: row.get(5)?,
        outcome: row.get(6)?,
        latency_ms: row.get(7)?,
        error: row.get(8)?,
    })
}

fn validate_client_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CompanionError::InvalidConfig(
            "API client 名称不能为空".into(),
        ));
    }
    if name.chars().count() > MAX_CLIENT_NAME_LEN {
        return Err(CompanionError::InvalidConfig(format!(
            "API client 名称不能超过 {MAX_CLIENT_NAME_LEN} 个字符"
        )));
    }
    Ok(name.to_string())
}

fn normalize_models(models: Vec<String>) -> Result<Vec<String>> {
    let models = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<BTreeSet<_>>();
    if models.len() > MAX_ALLOWED_MODELS {
        return Err(CompanionError::InvalidConfig(format!(
            "每个 API client 最多允许 {MAX_ALLOWED_MODELS} 个模型"
        )));
    }
    if let Some(model) = models
        .iter()
        .find(|model| model.chars().count() > MAX_MODEL_NAME_LEN)
    {
        return Err(CompanionError::InvalidConfig(format!(
            "模型名称过长: {model}"
        )));
    }
    Ok(models.into_iter().collect())
}

fn generate_api_key() -> String {
    let mut secret = [0_u8; 32];
    rand::rng().fill_bytes(&mut secret);
    format!("cc_live_{}", URL_SAFE_NO_PAD.encode(secret))
}

fn generate_client_id() -> String {
    let mut suffix = [0_u8; 6];
    rand::rng().fill_bytes(&mut suffix);
    format!(
        "client_{}_{}",
        Utc::now().timestamp_millis(),
        URL_SAFE_NO_PAD.encode(suffix).to_ascii_lowercase()
    )
}

fn key_prefix(api_key: &str) -> String {
    api_key.chars().take(API_KEY_PREFIX_LEN).collect()
}

fn hash_key(api_key: &str) -> [u8; 32] {
    Sha256::digest(api_key.as_bytes()).into()
}

fn compact_log_error(value: &str) -> String {
    let redacted = redact_sensitive_text(value);
    let compact = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(MAX_LOG_ERROR_LEN).collect()
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| CompanionError::InvalidConfig(format!("无效时间戳 {value}: {error}")))
}

fn parse_timestamp_sql(index: usize, value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn database_error(error: rusqlite::Error) -> CompanionError {
    CompanionError::InvalidConfig(format!("API service database error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, ApiServiceStore) {
        let temp = tempfile::tempdir().expect("temp");
        let config = ConfigStore::new(temp.path().join("config.json"));
        let store = ApiServiceStore::from_config_store(&config);
        store.initialize().expect("initialize");
        (temp, store)
    }

    #[test]
    fn client_key_is_shown_once_and_authenticates_by_hash() {
        let (_temp, store) = test_store();
        let created = store
            .create_client(ApiClientCreate {
                name: "Local app".into(),
                allowed_models: vec!["gpt-5.6".into()],
            })
            .expect("create");

        assert!(created.api_key.starts_with("cc_live_"));
        assert_eq!(created.client.allowed_models, vec!["gpt-5.6"]);
        let authenticated = store
            .authenticate(&created.api_key)
            .expect("authenticate")
            .expect("client");
        assert_eq!(authenticated.id, created.client.id);
        assert_eq!(authenticated.request_count, 1);
        assert!(store
            .authenticate("cc_live_wrong")
            .expect("invalid")
            .is_none());
        let clients = store.list_clients().expect("clients");
        assert_eq!(clients.len(), 1);
    }

    #[test]
    fn rotation_invalidates_previous_key() {
        let (_temp, store) = test_store();
        let created = store
            .create_client(ApiClientCreate {
                name: "CLI".into(),
                allowed_models: Vec::new(),
            })
            .expect("create");
        let rotated = store.rotate_client_key(&created.client.id).expect("rotate");

        assert!(store.authenticate(&created.api_key).expect("old").is_none());
        assert!(store.authenticate(&rotated.api_key).expect("new").is_some());
    }

    #[test]
    fn request_log_and_model_cooldown_are_persistent() {
        let (_temp, store) = test_store();
        store
            .record_request_start(RequestLogStart {
                request_id: "request-1",
                method: "POST",
                path: "/v1/responses",
                model: Some("gpt-test"),
                client_id: None,
            })
            .expect("start");
        store
            .record_request_attempt_start(RequestAttemptStart {
                request_id: "request-1",
                attempt: 1,
                provider_id: "provider-a",
                route_reason: "policy",
            })
            .expect("first attempt start");
        store
            .record_request_attempt_finish(RequestAttemptFinish {
                request_id: "request-1",
                attempt: 1,
                status_code: Some(503),
                outcome: "failed",
                latency_ms: 12,
                error: Some("temporarily unavailable"),
            })
            .expect("first attempt finish");
        store
            .record_request_attempt_start(RequestAttemptStart {
                request_id: "request-1",
                attempt: 2,
                provider_id: "provider-b",
                route_reason: "fallback",
            })
            .expect("second attempt start");
        store
            .record_request_attempt_finish(RequestAttemptFinish {
                request_id: "request-1",
                attempt: 2,
                status_code: Some(200),
                outcome: "succeeded",
                latency_ms: 30,
                error: None,
            })
            .expect("second attempt finish");
        store
            .record_request_finish(RequestLogFinish {
                request_id: "request-1",
                provider_id: Some("provider-b"),
                status_code: Some(200),
                outcome: "succeeded",
                attempts: 2,
                latency_ms: 42,
                error: None,
            })
            .expect("finish");
        store
            .set_model_cooldown("provider-a", "gpt-test", "rate limited", 60)
            .expect("cooldown");

        let snapshot = store.snapshot(100).expect("snapshot");
        assert_eq!(snapshot.recent_requests.len(), 1);
        assert_eq!(snapshot.recent_requests[0].attempts, 2);
        assert_eq!(snapshot.recent_requests[0].attempt_log.len(), 2);
        assert_eq!(
            snapshot.recent_requests[0].attempt_log[0].provider_id,
            "provider-a"
        );
        assert_eq!(
            snapshot.recent_requests[0].attempt_log[0].error.as_deref(),
            Some("temporarily unavailable")
        );
        assert_eq!(
            snapshot.recent_requests[0].attempt_log[1].route_reason,
            "fallback"
        );
        assert_eq!(snapshot.model_cooldowns.len(), 1);
        assert!(store
            .model_cooldown_active("provider-a", "gpt-test")
            .expect("active"));
    }

    #[test]
    fn request_log_redacts_credentials_in_paths_and_errors() {
        let (_temp, store) = test_store();
        store
            .record_request_start(RequestLogStart {
                request_id: "secret-request",
                method: "POST",
                path: "/v1/responses?api_key=sk-path-secret&safe=value",
                model: Some("gpt-test"),
                client_id: None,
            })
            .expect("start");
        store
            .record_request_attempt_start(RequestAttemptStart {
                request_id: "secret-request",
                attempt: 1,
                provider_id: "provider-a",
                route_reason: "policy",
            })
            .expect("attempt start");
        store
            .record_request_attempt_finish(RequestAttemptFinish {
                request_id: "secret-request",
                attempt: 1,
                status_code: Some(401),
                outcome: "failed",
                latency_ms: 2,
                error: Some("Authorization: Bearer sk-attempt-secret"),
            })
            .expect("attempt finish");
        store
            .record_request_finish(RequestLogFinish {
                request_id: "secret-request",
                provider_id: Some("provider-a"),
                status_code: Some(401),
                outcome: "failed",
                attempts: 1,
                latency_ms: 3,
                error: Some(r#"{"password":"database-secret"}"#),
            })
            .expect("finish");

        let snapshot = store.snapshot(10).expect("snapshot");
        let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
        assert!(!serialized.contains("sk-path-secret"));
        assert!(!serialized.contains("sk-attempt-secret"));
        assert!(!serialized.contains("database-secret"));
        assert!(serialized.contains("safe=value"));
        assert!(serialized.contains("[redacted]"));
    }

    #[test]
    fn client_calendar_usage_and_affinity_survive_store_reopen() {
        let (_temp, store) = test_store();
        let created = store
            .create_client(ApiClientCreate {
                name: "Automation".into(),
                allowed_models: Vec::new(),
            })
            .expect("client");
        for (index, outcome) in ["succeeded", "failed", "local"].iter().enumerate() {
            let request_id = format!("period-{index}");
            store
                .record_request_start(RequestLogStart {
                    request_id: &request_id,
                    method: "POST",
                    path: "/v1/responses",
                    model: Some("gpt-test"),
                    client_id: Some(&created.client.id),
                })
                .expect("start");
            store
                .record_request_finish(RequestLogFinish {
                    request_id: &request_id,
                    provider_id: Some("provider-a"),
                    status_code: Some(if *outcome == "failed" { 500 } else { 200 }),
                    outcome,
                    attempts: 1,
                    latency_ms: 30,
                    error: None,
                })
                .expect("finish");
        }
        store
            .bind_affinity("session-hash", "provider-a")
            .expect("affinity");

        let reopened = ApiServiceStore {
            path: store.path.clone(),
        };
        let snapshot = reopened.snapshot(10).expect("snapshot");
        let client = &snapshot.clients[0];
        assert_eq!(client.usage.today.requests, 3);
        assert_eq!(client.usage.today.succeeded, 2);
        assert_eq!(client.usage.today.failed, 1);
        assert_eq!(client.usage.today.success_rate, 66);
        assert_eq!(snapshot.affinity_bindings, 1);
        assert_eq!(
            reopened
                .preferred_affinity("session-hash", 3600)
                .expect("preferred")
                .as_deref(),
            Some("provider-a")
        );
    }

    #[test]
    fn chat_history_survives_store_reopen() {
        let (_temp, store) = test_store();
        let messages = serde_json::json!([{"role":"user","content":"hello"}]);
        let tools = serde_json::json!({"chatTools":[],"specs":[]});
        store
            .store_chat_history("provider-a", "resp-a", &messages, &tools)
            .expect("store");
        let reopened = ApiServiceStore {
            path: store.path.clone(),
        };
        let (stored_messages, stored_tools) = reopened
            .load_chat_history("provider-a", "resp-a")
            .expect("load")
            .expect("entry");
        assert_eq!(stored_messages, messages);
        assert_eq!(stored_tools, tools);
    }
}
