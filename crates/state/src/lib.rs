mod token_usage;

use chrono::Local;
use codex_companion_core::{
    default_codex_dir, CodexInstallStatus, CompanionError, ProviderConfig, RelayConfig,
    RepairOptions, RepairOutcome, RepairPlan, Result, COMPANION_PROVIDER_ID,
};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item};
use walkdir::WalkDir;

const CODEX_STATE_DB_FILENAME: &str = "state_5.sqlite";

pub use token_usage::{collect_token_usage, collect_token_usage_cached};

#[derive(Debug, Clone, Default)]
struct SessionMetadata {
    cwd: Option<String>,
    rollout_path: Option<String>,
}

pub fn install_companion_provider(
    codex_dir: Option<PathBuf>,
    relay: &RelayConfig,
) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    fs::create_dir_all(&codex_dir).map_err(|source| CompanionError::io(&codex_dir, source))?;
    let config_path = codex_dir.join("config.toml");
    let current = if config_path.exists() {
        fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?
    } else {
        String::new()
    };
    let mut doc = current.parse::<DocumentMut>().map_err(|source| {
        CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
    })?;

    doc["model_provider"] = value(COMPANION_PROVIDER_ID);
    doc["model_providers"][COMPANION_PROVIDER_ID]["name"] = value("Codex Companion");
    doc["model_providers"][COMPANION_PROVIDER_ID]["base_url"] = value(relay.base_url());
    doc["model_providers"][COMPANION_PROVIDER_ID]["wire_api"] = value("responses");

    fs::write(&config_path, doc.to_string())
        .map_err(|source| CompanionError::io(&config_path, source))?;
    doctor(codex_dir, relay)
}

pub fn install_direct_provider(
    codex_dir: Option<PathBuf>,
    provider: &ProviderConfig,
) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    fs::create_dir_all(&codex_dir).map_err(|source| CompanionError::io(&codex_dir, source))?;
    let config_path = codex_dir.join("config.toml");
    let current = if config_path.exists() {
        fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?
    } else {
        String::new()
    };
    let mut doc = current.parse::<DocumentMut>().map_err(|source| {
        CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
    })?;

    doc["model_provider"] = value(&provider.id);
    doc["model_providers"][&provider.id]["name"] = value(&provider.name);
    doc["model_providers"][&provider.id]["base_url"] = value(&provider.base_url);
    doc["model_providers"][&provider.id]["wire_api"] = value("responses");
    doc["model_providers"][&provider.id]["requires_openai_auth"] = value(true);
    doc["model_providers"][&provider.id]["api_key_env_var"] = Item::None;

    match resolve_direct_auth(provider)? {
        DirectAuthMaterial::EnvKey(env_var) => {
            doc["model_providers"][&provider.id]["env_key"] = value(env_var);
        }
        DirectAuthMaterial::ApiKey(api_key) => {
            doc["model_providers"][&provider.id]["env_key"] = Item::None;
            write_codex_openai_api_key(&codex_dir, &api_key)?;
        }
        DirectAuthMaterial::None => {
            doc["model_providers"][&provider.id]["env_key"] = Item::None;
        }
    }

    fs::write(&config_path, doc.to_string())
        .map_err(|source| CompanionError::io(&config_path, source))?;
    Ok(CodexInstallStatus {
        codex_dir,
        config_path,
        installed: true,
        model_provider: Some(provider.id.clone()),
        companion_base_url: provider.base_url.clone(),
        message: format!("Codex 已直连 provider: {}", provider.name),
    })
}

enum DirectAuthMaterial {
    EnvKey(String),
    ApiKey(String),
    None,
}

fn resolve_direct_auth(provider: &ProviderConfig) -> Result<DirectAuthMaterial> {
    let Some(auth_ref) = provider
        .direct_auth_ref
        .as_deref()
        .or(provider.auth_ref.as_deref())
        .map(str::trim)
        .filter(|auth_ref| !auth_ref.is_empty())
    else {
        return Ok(DirectAuthMaterial::None);
    };
    if let Some(env_var) = auth_ref
        .strip_prefix("env:")
        .map(str::trim)
        .filter(|env_var| !env_var.is_empty())
    {
        return Ok(DirectAuthMaterial::EnvKey(env_var.to_string()));
    }
    if let Some(path) = auth_ref.strip_prefix("file:") {
        let path = PathBuf::from(path);
        let text = fs::read_to_string(&path).map_err(|source| CompanionError::io(&path, source))?;
        let value = serde_json::from_str::<Value>(&text).map_err(|source| {
            CompanionError::InvalidConfig(format!(
                "解析 API Key auth 文件失败 {}: {source}",
                path.display()
            ))
        })?;
        let api_key = pick_json_string(
            &value,
            &[
                &["OPENAI_API_KEY"],
                &["api_key"],
                &["openai_api_key"],
                &["credentials", "api_key"],
                &["tokens", "api_key"],
            ],
        )
        .ok_or_else(|| {
            CompanionError::InvalidConfig(format!(
                "API Key auth 文件缺少 OPENAI_API_KEY/api_key: {}",
                path.display()
            ))
        })?;
        return Ok(DirectAuthMaterial::ApiKey(api_key));
    }
    Ok(DirectAuthMaterial::None)
}

fn write_codex_openai_api_key(codex_dir: &Path, api_key: &str) -> Result<()> {
    let auth_path = codex_dir.join("auth.json");
    let mut auth = if auth_path.exists() {
        let text = fs::read_to_string(&auth_path)
            .map_err(|source| CompanionError::io(&auth_path, source))?;
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };
    if !auth.is_object() {
        auth = Value::Object(Default::default());
    }
    let Some(object) = auth.as_object_mut() else {
        return Err(CompanionError::InvalidConfig(
            "Codex auth.json 不是 JSON object".to_string(),
        ));
    };
    object.insert(
        "OPENAI_API_KEY".to_string(),
        Value::String(api_key.to_string()),
    );
    let text = serde_json::to_string_pretty(&auth).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化 Codex auth.json 失败: {source}"))
    })?;
    fs::write(&auth_path, format!("{text}\n"))
        .map_err(|source| CompanionError::io(&auth_path, source))
}

fn pick_json_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
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

pub fn uninstall_companion_provider(codex_dir: Option<PathBuf>) -> Result<CodexInstallStatus> {
    let codex_dir = codex_dir.unwrap_or(default_codex_dir()?);
    let config_path = codex_dir.join("config.toml");
    if config_path.exists() {
        let current = fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?;
        let mut doc = current.parse::<DocumentMut>().map_err(|source| {
            CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
        })?;
        if doc
            .get("model_provider")
            .and_then(Item::as_str)
            .is_some_and(|provider| provider == COMPANION_PROVIDER_ID)
        {
            doc["model_provider"] = value("openai");
        }
        doc["model_providers"][COMPANION_PROVIDER_ID] = Item::None;
        fs::write(&config_path, doc.to_string())
            .map_err(|source| CompanionError::io(&config_path, source))?;
    }
    doctor(codex_dir, &RelayConfig::default())
}

pub fn doctor(codex_dir: PathBuf, relay: &RelayConfig) -> Result<CodexInstallStatus> {
    let config_path = codex_dir.join("config.toml");
    let mut model_provider = None;
    let mut installed = false;
    if config_path.exists() {
        let current = fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?;
        if let Ok(doc) = current.parse::<DocumentMut>() {
            model_provider = doc
                .get("model_provider")
                .and_then(Item::as_str)
                .map(ToOwned::to_owned);
            installed = model_provider.as_deref() == Some(COMPANION_PROVIDER_ID)
                && doc
                    .get("model_providers")
                    .and_then(|item| item.get(COMPANION_PROVIDER_ID))
                    .and_then(|item| item.get("base_url"))
                    .and_then(Item::as_str)
                    .is_some_and(|value| value == relay.base_url());
        }
    }
    let message = if installed {
        "Codex 已配置为使用 Codex Companion".to_string()
    } else if let Some(provider) = model_provider.as_deref() {
        format!("Codex 当前配置 provider: {provider}")
    } else if config_path.exists() {
        "Codex 配置存在，但尚未设置 model_provider".to_string()
    } else {
        "Codex 配置尚未创建，可在设置里写入 Companion 配置".to_string()
    };
    Ok(CodexInstallStatus {
        codex_dir,
        config_path,
        installed,
        model_provider,
        companion_base_url: relay.base_url(),
        message,
    })
}

pub fn repair_state(options: RepairOptions) -> Result<RepairOutcome> {
    if !options.history && !options.plugins {
        return Err(CompanionError::InvalidConfig(
            "修复需要指定 --history、--plugins 或同时指定两者".to_string(),
        ));
    }
    if !options.codex_dir.exists() {
        return Err(CompanionError::InvalidConfig(format!(
            "Codex 目录不存在: {}",
            options.codex_dir.display()
        )));
    }
    let target_provider_id = resolve_repair_target_provider_id(
        &options.codex_dir,
        options.target_provider_id.as_deref(),
    )?;

    let jsonl_files = if options.history {
        collect_files(&options.codex_dir, "jsonl")
    } else {
        Vec::new()
    };
    let plugin_files = if options.plugins {
        collect_plugin_json_files(&options.codex_dir)
    } else {
        Vec::new()
    };
    let db_path = options.codex_dir.join(CODEX_STATE_DB_FILENAME);

    let mut source_provider_ids = collect_jsonl_provider_ids(&jsonl_files)?;
    if options.history && db_path.exists() {
        source_provider_ids.extend(collect_sqlite_provider_ids(&db_path)?);
    }
    if options.plugins {
        source_provider_ids.extend(collect_plugin_provider_ids(&plugin_files)?);
    }
    source_provider_ids.remove(target_provider_id.as_str());

    let session_metadata = if options.history {
        collect_session_metadata(&jsonl_files)?
    } else {
        Default::default()
    };
    let state_rows = if options.history && db_path.exists() {
        count_sqlite_rows_to_repair(
            &db_path,
            &source_provider_ids,
            &target_provider_id,
            &session_metadata,
        )?
    } else {
        0
    };
    let history_lines = if options.history {
        count_jsonl_lines_to_migrate(&jsonl_files, &source_provider_ids)?
    } else {
        0
    };

    let plan = RepairPlan {
        codex_dir: options.codex_dir.clone(),
        target_provider_id: target_provider_id.clone(),
        history_files: jsonl_files.len(),
        history_lines,
        plugin_files: plugin_files.len(),
        state_rows,
        source_provider_ids: source_provider_ids.iter().cloned().collect(),
        dry_run: options.dry_run,
    };

    if source_provider_ids.is_empty() && state_rows == 0 {
        return Ok(RepairOutcome {
            plan,
            backup_root: None,
            migrated_history_files: 0,
            migrated_history_lines: 0,
            migrated_plugin_files: 0,
            migrated_state_rows: 0,
            skipped_reason: Some(format!(
                "未发现需要迁移到 {target_provider_id} 的 provider namespace"
            )),
        });
    }

    if options.dry_run {
        return Ok(RepairOutcome {
            plan,
            backup_root: None,
            migrated_history_files: 0,
            migrated_history_lines: 0,
            migrated_plugin_files: 0,
            migrated_state_rows: 0,
            skipped_reason: None,
        });
    }

    let migrated_history_files = 0;
    let migrated_history_lines = 0;
    let mut migrated_plugin_files = 0;
    let mut migrated_state_rows = 0;
    let mut backup_root = None;

    if options.history && db_path.exists() && state_rows > 0 {
        let backup_root = ensure_backup_root(&mut backup_root, &options.codex_dir)?;
        backup_file(&db_path, backup_root, &options.codex_dir)?;
        migrated_state_rows = repair_sqlite_threads(
            &db_path,
            &source_provider_ids,
            &target_provider_id,
            &session_metadata,
        )?;
    }

    for path in &plugin_files {
        if rewrite_plugin_file(
            path,
            &source_provider_ids,
            &target_provider_id,
            &mut backup_root,
            &options.codex_dir,
        )? {
            migrated_plugin_files += 1;
        }
    }

    let skipped_reason = if history_lines > 0 {
        Some("已保留历史会话文件不改写；仅合并 Codex SQLite 索引和插件状态".to_string())
    } else if migrated_plugin_files == 0 && migrated_state_rows == 0 {
        Some(format!(
            "未发现需要迁移到 {target_provider_id} 的可合并状态"
        ))
    } else {
        None
    };

    Ok(RepairOutcome {
        plan,
        backup_root,
        migrated_history_files,
        migrated_history_lines,
        migrated_plugin_files,
        migrated_state_rows,
        skipped_reason,
    })
}

fn resolve_repair_target_provider_id(codex_dir: &Path, requested: Option<&str>) -> Result<String> {
    if let Some(provider_id) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(provider_id.to_string());
    }

    let config_path = codex_dir.join("config.toml");
    if config_path.exists() {
        let current = fs::read_to_string(&config_path)
            .map_err(|source| CompanionError::io(&config_path, source))?;
        let doc = current.parse::<DocumentMut>().map_err(|source| {
            CompanionError::InvalidConfig(format!("invalid Codex config TOML: {source}"))
        })?;
        if let Some(provider_id) = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(provider_id.to_string());
        }
    }

    Ok(COMPANION_PROVIDER_ID.to_string())
}

fn collect_files(root: &Path, extension: &str) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_repair_backup_path(entry.path()))
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().is_some_and(|value| value == extension))
        .collect()
}

fn is_repair_backup_path(path: &Path) -> bool {
    let parts = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    parts
        .windows(2)
        .any(|window| window[0] == "backups" && window[1] == "codex-companion")
}

fn collect_plugin_json_files(root: &Path) -> Vec<PathBuf> {
    let known_dirs = [
        "plugins",
        "plugin-state",
        "plugin_state",
        "mcp",
        "mcp_state",
    ];
    known_dirs
        .iter()
        .map(|dir| root.join(dir))
        .filter(|dir| dir.exists())
        .flat_map(|dir| collect_files(&dir, "json"))
        .collect()
}

fn collect_plugin_provider_ids(files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            collect_provider_fields(&value, &mut ids);
        }
    }
    Ok(ids)
}

fn collect_provider_fields(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if is_provider_field_key(key) {
                    if let Some(current) = child.as_str().filter(|value| !value.is_empty()) {
                        ids.insert(current.to_string());
                        continue;
                    }
                }
                collect_provider_fields(child, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_provider_fields(item, ids);
            }
        }
        _ => {}
    }
}

fn collect_jsonl_provider_ids(files: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            if let Some(provider) = session_meta_provider(&line) {
                ids.insert(provider);
            }
        }
    }
    Ok(ids)
}

fn collect_session_metadata(files: &[PathBuf]) -> Result<BTreeMap<String, SessionMetadata>> {
    let mut metadata = BTreeMap::new();
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            let Some((id, mut entry)) = session_meta_metadata(&line) else {
                continue;
            };
            entry
                .rollout_path
                .get_or_insert_with(|| path.display().to_string());
            metadata
                .entry(id)
                .and_modify(|current: &mut SessionMetadata| current.merge_missing(&entry))
                .or_insert(entry);
        }
    }
    Ok(metadata)
}

impl SessionMetadata {
    fn merge_missing(&mut self, other: &SessionMetadata) {
        if self.cwd.as_deref().is_none_or(str::is_empty) {
            self.cwd = other.cwd.clone();
        }
        if self.rollout_path.as_deref().is_none_or(str::is_empty) {
            self.rollout_path = other.rollout_path.clone();
        }
    }
}

fn session_meta_provider(line: &str) -> Option<String> {
    if !line.contains("session_meta") || !line.contains("model_provider") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("payload")?
        .get("model_provider")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn session_meta_metadata(line: &str) -> Option<(String, SessionMetadata)> {
    if !line.contains("session_meta") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;
    let id = payload
        .get("id")?
        .as_str()
        .filter(|value| !value.is_empty())?
        .to_string();
    Some((
        id,
        SessionMetadata {
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            rollout_path: payload
                .get("rollout_path")
                .or_else(|| payload.get("rolloutPath"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        },
    ))
}

fn count_jsonl_lines_to_migrate(files: &[PathBuf], source_ids: &BTreeSet<String>) -> Result<usize> {
    if source_ids.is_empty() {
        return Ok(0);
    }
    let mut count = 0;
    for path in files {
        let file = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|source| CompanionError::io(path, source))?;
            if session_meta_provider(&line).is_some_and(|provider| source_ids.contains(&provider)) {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn collect_sqlite_provider_ids(path: &Path) -> Result<BTreeSet<String>> {
    let conn = open_sqlite(path)?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT model_provider FROM threads WHERE model_provider IS NOT NULL")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let values = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let mut ids = BTreeSet::new();
    for value in values {
        let value = value.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite row failed: {source}"))
        })?;
        if !value.is_empty() {
            ids.insert(value);
        }
    }
    Ok(ids)
}

fn count_sqlite_rows_to_repair(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: &BTreeMap<String, SessionMetadata>,
) -> Result<usize> {
    let conn = open_sqlite(path)?;
    if !sqlite_threads_have_repair_columns(&conn)? {
        return Ok(0);
    }
    let mut stmt = conn
        .prepare("SELECT id, model_provider, cwd, rollout_path FROM threads")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SqliteThreadRow {
                id: row.get(0)?,
                model_provider: row.get(1)?,
                cwd: row.get(2)?,
                rollout_path: row.get(3)?,
            })
        })
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let mut total = 0;
    for row in rows {
        let row = row.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite row failed: {source}"))
        })?;
        if sqlite_thread_needs_repair(&row, source_ids, target_provider_id, metadata.get(&row.id)) {
            total += 1;
        }
    }
    Ok(total)
}

fn repair_sqlite_threads(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: &BTreeMap<String, SessionMetadata>,
) -> Result<usize> {
    let conn = open_sqlite(path)?;
    if !sqlite_threads_have_repair_columns(&conn)? {
        return Ok(0);
    }
    let mut total = 0;
    let mut stmt = conn
        .prepare("SELECT id, model_provider, cwd, rollout_path FROM threads")
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite query failed: {source}"))
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SqliteThreadRow {
                id: row.get(0)?,
                model_provider: row.get(1)?,
                cwd: row.get(2)?,
                rollout_path: row.get(3)?,
            })
        })
        .map_err(|source| CompanionError::InvalidConfig(format!("SQLite query failed: {source}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| CompanionError::InvalidConfig(format!("SQLite row failed: {source}")))?;
    drop(stmt);

    for row in rows {
        let row_metadata = metadata.get(&row.id);
        if !sqlite_thread_needs_repair(&row, source_ids, target_provider_id, row_metadata) {
            continue;
        }
        let next_provider = if source_ids.contains(&row.model_provider) {
            target_provider_id.to_string()
        } else {
            row.model_provider.clone()
        };
        let next_cwd = merge_string_field(
            &row.cwd,
            row_metadata.and_then(|value| value.cwd.as_deref()),
        );
        let next_rollout_path = merge_string_field(
            &row.rollout_path,
            row_metadata.and_then(|value| value.rollout_path.as_deref()),
        );
        total += conn
            .execute(
                "UPDATE threads SET model_provider = ?, cwd = ?, rollout_path = ? WHERE id = ?",
                params![next_provider, next_cwd, next_rollout_path, row.id],
            )
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("SQLite update failed: {source}"))
            })?;
    }
    Ok(total)
}

#[derive(Debug)]
struct SqliteThreadRow {
    id: String,
    model_provider: String,
    cwd: String,
    rollout_path: String,
}

fn sqlite_thread_needs_repair(
    row: &SqliteThreadRow,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    metadata: Option<&SessionMetadata>,
) -> bool {
    if source_ids.contains(&row.model_provider) {
        return true;
    }
    if row.model_provider != target_provider_id {
        return false;
    }
    metadata.is_some_and(|metadata| {
        string_field_needs_merge(&row.cwd, metadata.cwd.as_deref())
            || string_field_needs_merge(&row.rollout_path, metadata.rollout_path.as_deref())
    })
}

fn string_field_needs_merge(current: &str, incoming: Option<&str>) -> bool {
    current.trim().is_empty() && incoming.is_some_and(|value| !value.trim().is_empty())
}

fn merge_string_field(current: &str, incoming: Option<&str>) -> String {
    if string_field_needs_merge(current, incoming) {
        incoming.unwrap_or_default().to_string()
    } else {
        current.to_string()
    }
}

fn sqlite_table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })?;
    for value in columns {
        if value.map_err(|source| {
            CompanionError::InvalidConfig(format!("SQLite schema failed: {source}"))
        })? == column
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_threads_have_repair_columns(conn: &Connection) -> Result<bool> {
    for column in ["id", "model_provider", "cwd", "rollout_path"] {
        if !sqlite_table_has_column(conn, "threads", column)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn open_sqlite(path: &Path) -> Result<Connection> {
    Connection::open(path).map_err(|source| {
        CompanionError::InvalidConfig(format!(
            "failed to open SQLite at {}: {source}",
            path.display()
        ))
    })
}

fn rewrite_plugin_file(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
    backup_root: &mut Option<PathBuf>,
    codex_dir: &Path,
) -> Result<bool> {
    let text = fs::read_to_string(path).map_err(|source| CompanionError::io(path, source))?;
    let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
        return Ok(false);
    };
    if rewrite_provider_fields(&mut value, source_ids, target_provider_id) {
        let backup_root = ensure_backup_root(backup_root, codex_dir)?;
        backup_file(path, backup_root, codex_dir)?;
        let next = serde_json::to_string_pretty(&value)
            .map_err(|source| CompanionError::json(path, source))?;
        fs::write(path, format!("{next}\n")).map_err(|source| CompanionError::io(path, source))?;
        return Ok(true);
    }
    Ok(false)
}

fn rewrite_provider_fields(
    value: &mut Value,
    source_ids: &BTreeSet<String>,
    target_provider_id: &str,
) -> bool {
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (key, child) in map.iter_mut() {
                if is_provider_field_key(key) {
                    if let Some(current) = child.as_str() {
                        if source_ids.contains(current) {
                            *child = Value::String(target_provider_id.to_string());
                            changed = true;
                            continue;
                        }
                    }
                }
                changed |= rewrite_provider_fields(child, source_ids, target_provider_id);
            }
            changed
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            rewrite_provider_fields(item, source_ids, target_provider_id) || changed
        }),
        _ => false,
    }
}

fn is_provider_field_key(key: &str) -> bool {
    matches!(
        key,
        "provider" | "provider_id" | "providerId" | "model_provider" | "modelProvider"
    )
}

fn create_backup_root(codex_dir: &Path) -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let root = codex_dir
        .join("backups")
        .join("codex-companion")
        .join(timestamp);
    fs::create_dir_all(&root).map_err(|source| CompanionError::io(&root, source))?;
    Ok(root)
}

fn ensure_backup_root<'a>(
    backup_root: &'a mut Option<PathBuf>,
    codex_dir: &Path,
) -> Result<&'a Path> {
    if backup_root.is_none() {
        *backup_root = Some(create_backup_root(codex_dir)?);
    }
    Ok(backup_root.as_deref().expect("backup root"))
}

fn backup_file(path: &Path, backup_root: &Path, codex_dir: &Path) -> Result<()> {
    let relative = path.strip_prefix(codex_dir).unwrap_or(path);
    let target = backup_root.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
    }
    fs::copy(path, &target).map_err(|source| CompanionError::io(path, source))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_companion_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let relay = RelayConfig::default();
        let status =
            install_companion_provider(Some(temp.path().to_path_buf()), &relay).expect("install");
        assert!(status.installed);
        let text = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(text.contains("model_provider = \"codex-companion\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:17687/v1\""));
    }

    #[test]
    fn dry_run_does_not_write_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: true,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.source_provider_ids, vec!["openai".to_string()]);
        assert_eq!(outcome.plan.history_files, 1);
        assert_eq!(outcome.plan.history_lines, 1);
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openai\""));
    }

    #[test]
    fn repair_ignores_its_own_backups() {
        let temp = tempfile::tempdir().expect("tempdir");
        let backup_sessions = temp
            .path()
            .join("backups")
            .join("codex-companion")
            .join("old")
            .join("sessions");
        fs::create_dir_all(&backup_sessions).expect("backup sessions");
        let backup_file = backup_sessions.join("session.jsonl");
        fs::write(
            &backup_file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert!(outcome.backup_root.is_none());
        assert!(outcome.skipped_reason.is_some());
        assert_eq!(outcome.plan.history_files, 0);
        assert_eq!(outcome.plan.history_lines, 0);
        let text = fs::read_to_string(&backup_file).expect("read");
        assert!(text.contains("\"openai\""));
    }

    #[test]
    fn repair_merges_sqlite_index_without_rewriting_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\",\"cwd\":\"/work/project\"}}\n",
        )
        .expect("write");

        let db = temp.path().join(CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, cwd TEXT NOT NULL, rollout_path TEXT NOT NULL);
             INSERT INTO threads (id, model_provider, cwd, rollout_path) VALUES ('s1', 'openai', '', '');",
        )
        .expect("schema");
        drop(conn);

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.history_lines, 1);
        assert_eq!(outcome.plan.state_rows, 1);
        assert_eq!(outcome.migrated_history_lines, 0);
        assert_eq!(outcome.migrated_state_rows, 1);
        assert!(outcome.backup_root.is_some());
        assert!(outcome.skipped_reason.is_some());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openai\""));
        let conn = Connection::open(&db).expect("db");
        let (provider, cwd): (String, String) = conn
            .query_row(
                "SELECT model_provider, cwd FROM threads WHERE id = 's1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("provider");
        assert_eq!(provider, COMPANION_PROVIDER_ID);
        assert_eq!(cwd, "/work/project");
    }

    #[test]
    fn repair_merges_missing_project_for_target_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"codex-companion\",\"cwd\":\"/work/project\"}}\n",
        )
        .expect("write");

        let db = temp.path().join(CODEX_STATE_DB_FILENAME);
        let conn = Connection::open(&db).expect("db");
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, cwd TEXT NOT NULL, rollout_path TEXT NOT NULL);
             INSERT INTO threads (id, model_provider, cwd, rollout_path) VALUES ('s1', 'codex-companion', '', '');",
        )
        .expect("schema");
        drop(conn);

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some(COMPANION_PROVIDER_ID.to_string()),
        })
        .expect("repair");

        assert!(outcome.backup_root.is_some());
        assert_eq!(outcome.plan.state_rows, 1);
        assert_eq!(outcome.migrated_history_lines, 0);
        assert_eq!(outcome.migrated_state_rows, 1);
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"codex-companion\""));
        let conn = Connection::open(&db).expect("db");
        let cwd: String = conn
            .query_row("SELECT cwd FROM threads WHERE id = 's1'", [], |row| {
                row.get(0)
            })
            .expect("cwd");
        assert_eq!(cwd, "/work/project");
    }

    #[test]
    fn repair_plans_custom_target_without_rewriting_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: Some("openrouter".to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.plan.target_provider_id, "openrouter");
        assert_eq!(outcome.migrated_history_lines, 0);
        assert!(outcome.backup_root.is_none());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openai\""));
    }

    #[test]
    fn repair_defaults_to_current_codex_model_provider_without_rewriting_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("config.toml"),
            "model_provider = \"openrouter\"\n",
        )
        .expect("config");
        let sessions = temp.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions");
        let file = sessions.join("session.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: true,
            plugins: false,
            dry_run: false,
            target_provider_id: None,
        })
        .expect("repair");

        assert_eq!(outcome.plan.target_provider_id, "openrouter");
        assert_eq!(outcome.migrated_history_lines, 0);
        assert!(outcome.backup_root.is_none());
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"openai\""));
    }

    #[test]
    fn plugin_only_repair_collects_provider_ids_from_plugins() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin_dir = temp.path().join("plugin-state");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");
        let file = plugin_dir.join("plugin.json");
        fs::write(
            &file,
            r#"{"provider":"openai","nested":{"modelProvider":"custom"}}"#,
        )
        .expect("write");

        let outcome = repair_state(RepairOptions {
            codex_dir: temp.path().to_path_buf(),
            history: false,
            plugins: true,
            dry_run: false,
            target_provider_id: Some("codex-companion".to_string()),
        })
        .expect("repair");

        assert_eq!(outcome.migrated_plugin_files, 1);
        assert_eq!(
            outcome.plan.source_provider_ids,
            vec!["custom".to_string(), "openai".to_string()]
        );
        let text = fs::read_to_string(&file).expect("read");
        assert!(text.contains(r#""provider": "codex-companion""#));
        assert!(text.contains(r#""modelProvider": "codex-companion""#));
    }

    #[test]
    fn install_direct_provider_writes_native_codex_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let provider = ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            auth_ref: Some("env:OPENROUTER_API_KEY".to_string()),
            direct_auth_ref: Some("env:OPENROUTER_API_KEY".to_string()),
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.installed);
        assert_eq!(status.model_provider.as_deref(), Some("openrouter"));
        let text = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(text.contains("model_provider = \"openrouter\""));
        assert!(text.contains("base_url = \"https://openrouter.ai/api/v1\""));
        assert!(text.contains("env_key = \"OPENROUTER_API_KEY\""));
        assert!(text.contains("requires_openai_auth = true"));
        assert!(!text.contains("api_key_env_var"));
    }

    #[test]
    fn install_direct_provider_with_file_key_writes_codex_auth_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_path = temp.path().join("provider-auth.json");
        fs::write(&auth_path, r#"{"api_key":"sk-test"}"#).expect("auth");
        let provider = ProviderConfig {
            id: "openrouter".to_string(),
            name: "OpenRouter".to_string(),
            kind: codex_companion_core::ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            auth_ref: Some(format!("file:{}", auth_path.display())),
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 100,
            enabled: true,
            refresh_interval_seconds: codex_companion_core::default_refresh_interval_seconds(),
            account: None,
        };

        let status = install_direct_provider(Some(temp.path().to_path_buf()), &provider)
            .expect("install direct");

        assert!(status.installed);
        let config = fs::read_to_string(temp.path().join("config.toml")).expect("config");
        assert!(config.contains("model_provider = \"openrouter\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(!config.contains("env_key ="));
        assert!(!config.contains("api_key_env_var"));
        let auth = fs::read_to_string(temp.path().join("auth.json")).expect("codex auth");
        assert!(auth.contains("\"OPENAI_API_KEY\": \"sk-test\""));
    }
}
