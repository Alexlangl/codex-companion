//! Repair disposable pagination projections only when durable rollouts prove
//! that their byte cursor is stale. Never rewrite the source conversation.
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const DATABASE: &str = "thread_history_1.sqlite";
const TABLES: [&str; 4] = [
    "thread_items",
    "thread_realtime_items",
    "thread_turns",
    "thread_history_projection_state",
];
const MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryIndexRepairReport {
    pub dry_run: bool,
    pub scanned_sessions: usize,
    pub affected_sessions: Vec<String>,
    pub repaired_sessions: usize,
    pub backup_path: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    digest: [u8; 32],
    identity: String,
    decoded: u64,
    initial: u64,
    next: Option<u64>,
    base: Option<(String, u64, u64)>,
}

struct ScanBudget {
    started: Instant,
    bytes: u64,
}
impl ScanBudget {
    fn consume(&mut self, bytes: u64) -> Result<(), String> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.started.elapsed() > Duration::from_secs(45) || self.bytes > 2 * 1024 * 1024 * 1024 {
            return Err("会话索引扫描超出范围，请缩小 Codex 目录后重试".into());
        }
        Ok(())
    }
}

fn regular(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("会话索引修复不接受符号链接或特殊文件".into());
    }
    Ok(metadata)
}

fn file_identity(path: &Path) -> Result<String, String> {
    let metadata = regular(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        metadata
            .created()
            .map(|at| format!("{at:?}"))
            .map_err(|e| e.to_string())
    }
}

fn id(value: &str) -> Option<String> {
    // Canonical UUID syntax without accepting path components or arbitrary SQL identifiers.
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn file_ids(path: &Path) -> Option<(String, String)> {
    let name = path
        .file_name()?
        .to_str()?
        .strip_suffix(".zst")
        .unwrap_or(path.file_name()?.to_str()?);
    let tail = name
        .strip_prefix("rollout-")?
        .strip_suffix(".jsonl")?
        .get(20..)?;
    let (logical, physical) = tail.split_once('_').unwrap_or((tail, tail));
    Some((id(logical)?, id(physical)?))
}

fn read_source(path: &Path, budget: &mut ScanBudget) -> Result<Source, String> {
    let metadata = regular(path)?;
    if metadata.len() > MAX_BYTES {
        return Err("会话文件过大，未修改索引".into());
    }
    let modified = metadata.modified().map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut raw = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buffer = [0; 65536];
    loop {
        let count = raw.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        budget.consume(count as u64)?;
        hash.update(&buffer[..count]);
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let input: Box<dyn Read> = if path.extension().is_some_and(|s| s == "zst") {
        let mut decoder = zstd::stream::read::Decoder::new(file).map_err(|e| e.to_string())?;
        decoder.window_log_max(27).map_err(|e| e.to_string())?;
        Box::new(decoder)
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::new(input);
    let mut line = Vec::new();
    let mut source = Source {
        path: path.into(),
        length: metadata.len(),
        modified,
        digest: hash.finalize().into(),
        identity: file_identity(path)?,
        decoded: 0,
        initial: 0,
        next: None,
        base: None,
    };
    let mut first = true;
    let mut expected = 0;
    let mut legacy = false;
    loop {
        line.clear();
        let count = reader
            .by_ref()
            .take(64 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        budget.consume(count as u64)?;
        source.decoded += count as u64;
        if count > 64 * 1024 * 1024 || source.decoded > MAX_BYTES || line.last() != Some(&b'\n') {
            return Err("会话文件过大或尾部尚未写完，未修改索引".into());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record: Value =
            serde_json::from_slice(&line).map_err(|_| "会话文件包含损坏记录，未修改索引")?;
        if first {
            let (logical, _) = file_ids(path).ok_or("会话文件名无效")?;
            if record["type"] != "session_meta"
                || record["payload"]["id"].as_str().and_then(id).as_deref() != Some(&logical)
            {
                return Err("会话身份与文件名不一致，未修改索引".into());
            }
            if let Some(base) = record["payload"]
                .get("history_base")
                .filter(|v| !v.is_null())
            {
                let parent = base["thread_id"]
                    .as_str()
                    .and_then(id)
                    .ok_or("祖先会话 ID 无效")?;
                let bytes = base["end_byte_offset"].as_u64().ok_or("祖先字节范围无效")?;
                expected = base["end_ordinal_exclusive"]
                    .as_u64()
                    .filter(|v| *v > 0)
                    .ok_or("祖先序号范围无效")?;
                source.initial = expected;
                source.base = Some((parent, bytes, expected));
            }
            first = false;
            if record.get("ordinal").is_none() {
                continue;
            }
        }
        if let Some(ordinal) = record["ordinal"].as_u64() {
            if legacy || ordinal != expected {
                return Err("会话序号不连续，未修改索引".into());
            }
            expected = ordinal.checked_add(1).ok_or("会话序号溢出")?;
            source.next = Some(expected);
        } else {
            if source.next.is_some() {
                return Err("会话缺少序号，未修改索引".into());
            }
            legacy = true;
        }
    }
    if first {
        return Err("会话文件为空，未修改索引".into());
    }
    check_stamp(&source)?;
    Ok(source)
}

fn check_stamp(source: &Source) -> Result<(), String> {
    let metadata = regular(&source.path)?;
    if file_identity(&source.path)? != source.identity
        || metadata.len() != source.length
        || metadata.modified().ok() != Some(source.modified)
    {
        return Err("会话正在变化，未修改索引，请稍后重试".into());
    }
    Ok(())
}

fn load_chain(
    thread: &str,
    registry: &BTreeMap<String, PathBuf>,
    sources: &mut BTreeMap<String, Source>,
    visiting: &mut BTreeSet<String>,
    budget: &mut ScanBudget,
) -> Result<(), String> {
    if sources.contains_key(thread) {
        return Ok(());
    }
    if visiting.len() >= 128 || !visiting.insert(thread.to_owned()) {
        return Err("会话祖先关系循环或过深，未修改索引".into());
    }
    let source = read_source(
        registry
            .get(thread)
            .ok_or("原始会话或祖先记录缺失，未修改索引")?,
        budget,
    )?;
    if let Some((parent, bytes, ordinal)) = &source.base {
        load_chain(parent, registry, sources, visiting, budget)?;
        let ancestor = &sources[parent];
        if *bytes > ancestor.decoded || ancestor.next.is_none_or(|next| *ordinal > next) {
            return Err("祖先会话记录不完整，未修改索引".into());
        }
    }
    visiting.remove(thread);
    sources.insert(thread.to_owned(), source);
    Ok(())
}

fn schema(db: &Connection) -> Result<String, String> {
    let mut statement = db.prepare("SELECT type,name,COALESCE(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").map_err(|e| e.to_string())?;
    let objects = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    for (kind, name, sql) in &objects {
        let cleanup: String = sql
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .trim_end_matches(';')
            .to_ascii_lowercase();
        if !(kind == "index" || kind == "table" && (TABLES.contains(&name.as_str()) || name == "_sqlx_migrations") || kind == "trigger" && name == "thread_realtime_items_projection_cleanup" && cleanup == "createtriggerthread_realtime_items_projection_cleanupafterdeleteonthread_history_projection_statebegindeletefromthread_realtime_itemswherethread_id=old.thread_id;end") { return Err("聊天索引结构不受支持，未修改数据".into()); }
    }
    for table in TABLES {
        if !objects.iter().any(|(kind, name, sql)| {
            kind == "table"
                && name == table
                && sql
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE ")
        }) {
            return Err("聊天索引结构不完整，未修改数据".into());
        }
        let has_thread: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='thread_id' AND type='TEXT')", [table], |r| r.get(0)).map_err(|e| e.to_string())?;
        let foreign_keys: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1))",
                [table],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !has_thread || foreign_keys {
            return Err("聊天索引存在不受支持的字段或引用，未修改数据".into());
        }
    }
    serde_json::to_string(&objects).map_err(|e| e.to_string())
}

fn cursors(db: &Connection) -> Result<Vec<(String, i64, i64)>, String> {
    let mut statement = db.prepare("SELECT thread_id,next_rollout_byte_offset,next_rollout_ordinal FROM thread_history_projection_state ORDER BY thread_id LIMIT 100001").map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    if rows.len() > 100_000 {
        return Err("会话索引过多，未修改数据".into());
    }
    Ok(rows)
}

pub fn repair_history_index(
    profile: &Path,
    dry_run: bool,
) -> Result<HistoryIndexRepairReport, String> {
    let root = profile.symlink_metadata().map_err(|e| e.to_string())?;
    if !root.is_dir() || root.file_type().is_symlink() {
        return Err("Codex 目录必须是普通目录".into());
    }
    let profile = profile.canonicalize().map_err(|e| e.to_string())?;
    let path = profile.join(DATABASE);
    let mut report = HistoryIndexRepairReport {
        dry_run,
        scanned_sessions: 0,
        affected_sessions: vec![],
        repaired_sessions: 0,
        backup_path: None,
    };
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(report);
    }
    regular(&path)?;
    let database_identity = file_identity(&path)?;
    for suffix in ["-wal", "-shm"] {
        let side = profile.join(format!("{DATABASE}{suffix}"));
        if side.symlink_metadata().is_ok() {
            regular(&side)?;
        }
    }
    let mut db = Connection::open_with_flags(
        &path,
        if dry_run {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        },
    )
    .map_err(|e| e.to_string())?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA trusted_schema=OFF")
        .map_err(|e| e.to_string())?;
    let version: i64 = db
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let original_schema = schema(&db)?;
    let original_cursors = cursors(&db)?;
    let mut budget = ScanBudget {
        started: Instant::now(),
        bytes: 0,
    };
    let mut registry = BTreeMap::new();
    for directory in ["sessions", "archived_sessions"] {
        let base = profile.join(directory);
        if !base.exists() {
            continue;
        }
        for (count, entry) in walkdir::WalkDir::new(base)
            .follow_links(false)
            .into_iter()
            .enumerate()
        {
            budget.consume(0)?;
            if count > 100_000 {
                return Err("会话文件过多，未修改数据".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_symlink() {
                return Err("会话目录包含符号链接，未修改索引".into());
            }
            if let Some((_, physical)) = file_ids(entry.path()) {
                regular(entry.path())?;
                if registry.insert(physical, entry.path().to_owned()).is_some() {
                    return Err("同一会话存在多个源文件，未修改索引".into());
                }
            }
        }
    }
    let mut sources = BTreeMap::new();
    for (thread, offset, ordinal) in &original_cursors {
        if *offset < 0 || *ordinal < 0 {
            return Err("聊天索引游标无效，未修改数据".into());
        }
        load_chain(
            thread,
            &registry,
            &mut sources,
            &mut BTreeSet::new(),
            &mut budget,
        )?;
        let source = &sources[thread];
        if *offset as u64 > source.decoded {
            if source.next.is_none_or(|next| *ordinal as u64 > next)
                || (*ordinal as u64) < source.initial
            {
                return Err("原始记录不足以重建索引，未修改数据".into());
            }
            report.affected_sessions.push(thread.clone());
        }
    }
    report.scanned_sessions = sources.len();
    if dry_run || report.affected_sessions.is_empty() {
        return Ok(report);
    }
    if report.affected_sessions.len() > 64 {
        return Err("异常索引超过单次修复范围，未修改数据".into());
    }
    // An online SQLite backup includes committed WAL contents. A raw file copy
    // here would lose records when the official client is running.
    let backup_dir = profile.join("backups/codex-companion/history-index");
    fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
    if backup_dir.canonicalize().map_err(|e| e.to_string())? != backup_dir {
        return Err("索引备份目录包含符号链接".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix("history-")
        .suffix(".sqlite")
        .tempfile_in(&backup_dir)
        .map_err(|e| e.to_string())?;
    let mut backup = Connection::open(temporary.path()).map_err(|e| e.to_string())?;
    {
        let copy = rusqlite::backup::Backup::new(&db, &mut backup).map_err(|e| e.to_string())?;
        for _ in 0..4096 {
            budget.consume(0)?;
            if temporary
                .as_file()
                .metadata()
                .map_err(|e| e.to_string())?
                .len()
                > 256 * 1024 * 1024
            {
                return Err("聊天索引备份超出范围，未修改数据".into());
            }
            match copy.step(256).map_err(|e| e.to_string())? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => continue,
                _ => return Err("聊天索引正忙，未修改数据，请稍后重试".into()),
            }
        }
        if copy.progress().remaining != 0 {
            return Err("聊天索引备份未完成，未修改数据".into());
        }
    }
    drop(backup);
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    for source in sources.values() {
        if read_source(&source.path, &mut budget)? != *source {
            return Err("会话内容已变化，未修改索引".into());
        }
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "聊天索引正忙，请稍后重试")?;
    let current: i64 = tx
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if file_identity(&path)? != database_identity
        || current != version
        || schema(&tx)? != original_schema
        || cursors(&tx)? != original_cursors
    {
        return Err("聊天索引已变化，未修改数据，请重新扫描".into());
    }
    let saved = temporary
        .into_temp_path()
        .keep()
        .map_err(|e| e.to_string())?;
    for thread in &report.affected_sessions {
        for table in TABLES {
            tx.execute(&format!("DELETE FROM {table} WHERE thread_id=?1"), [thread])
                .map_err(|e| e.to_string())?;
        }
    }
    for source in sources.values() {
        check_stamp(source)?;
    }
    if file_identity(&path)? != database_identity {
        return Err("聊天索引已被替换，未修改数据".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    report.repaired_sessions = report.affected_sessions.len();
    report.backup_path = Some(saved);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    const THREAD: &str = "11111111-1111-4111-8111-111111111111";
    const PARENT: &str = "22222222-2222-4222-8222-222222222222";
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sessions")).unwrap();
        let mut db = Connection::open(dir.path().join(DATABASE)).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE thread_items(thread_id TEXT NOT NULL,item_json TEXT);
            CREATE TABLE thread_realtime_items(thread_id TEXT NOT NULL,item_json TEXT);
            CREATE TABLE thread_turns(thread_id TEXT NOT NULL,status TEXT);
            CREATE TABLE thread_history_projection_state(thread_id TEXT PRIMARY KEY,next_rollout_byte_offset INTEGER NOT NULL,next_rollout_ordinal INTEGER NOT NULL);").unwrap();
        let tx = db.transaction().unwrap();
        for thread in [THREAD, PARENT] {
            let source = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{thread}\"}}}}\n{{\"ordinal\":0,\"type\":\"event_msg\",\"payload\":{{\"text\":\"durable fixture\"}}}}\n");
            fs::write(rollout(dir.path(), thread), source).unwrap();
            tx.execute("INSERT INTO thread_items VALUES (?1,'fixture')", [thread])
                .unwrap();
            tx.execute(
                "INSERT INTO thread_history_projection_state VALUES (?1,?2,1)",
                rusqlite::params![thread, if thread == THREAD { 99999 } else { 1 }],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        dir
    }
    fn rollout(root: &Path, thread: &str) -> PathBuf {
        root.join("sessions")
            .join(format!("rollout-2026-10-27T01-00-00-{thread}.jsonl"))
    }
    #[test]
    fn preview_is_read_only_and_repair_backs_up_wal_and_preserves_durable_history() {
        let dir = fixture();
        let original = fs::read(rollout(dir.path(), THREAD)).unwrap();
        let db = Connection::open(dir.path().join(DATABASE)).unwrap();
        // Keep a WAL writer connection alive throughout repair.
        db.execute(
            "UPDATE thread_items SET item_json='committed-wal' WHERE thread_id=?1",
            [THREAD],
        )
        .unwrap();
        let preview = repair_history_index(dir.path(), true).unwrap();
        assert_eq!(preview.affected_sessions, vec![THREAD]);
        assert_eq!(cursors(&db).unwrap().len(), 2);
        let report = repair_history_index(dir.path(), false).unwrap();
        assert_eq!(report.repaired_sessions, 1);
        assert_eq!(cursors(&db).unwrap().len(), 1);
        assert_eq!(cursors(&db).unwrap()[0].0, PARENT);
        let backup = Connection::open(report.backup_path.unwrap()).unwrap();
        let saved: String = backup
            .query_row(
                "SELECT item_json FROM thread_items WHERE thread_id=?1",
                [THREAD],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, "committed-wal");
        assert_eq!(fs::read(rollout(dir.path(), THREAD)).unwrap(), original);
        assert_eq!(
            repair_history_index(dir.path(), false)
                .unwrap()
                .repaired_sessions,
            0
        );
    }
    #[test]
    fn missing_ancestor_or_ordinal_evidence_never_deletes_projection() {
        for source in [
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{THREAD}\",\"history_base\":{{\"thread_id\":\"33333333-3333-4333-8333-333333333333\",\"end_byte_offset\":10,\"end_ordinal_exclusive\":1}}}}}}\n{{\"ordinal\":1,\"type\":\"event_msg\"}}\n"),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{THREAD}\"}}}}\n{{\"type\":\"event_msg\"}}\n"),
        ] {
            let dir = fixture(); fs::write(rollout(dir.path(),THREAD),source).unwrap();
            assert!(repair_history_index(dir.path(),false).is_err());
            assert_eq!(cursors(&Connection::open(dir.path().join(DATABASE)).unwrap()).unwrap().len(),2);
        }
    }
    #[test]
    fn compressed_sources_and_ancestor_cutoffs_are_verified() {
        let dir = fixture();
        let parent = rollout(dir.path(), PARENT);
        let data = fs::read(&parent).unwrap();
        fs::write(
            parent.with_extension("jsonl.zst"),
            zstd::stream::encode_all(data.as_slice(), 1).unwrap(),
        )
        .unwrap();
        fs::remove_file(parent).unwrap();
        let child = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{THREAD}\",\"history_base\":{{\"thread_id\":\"{PARENT}\",\"end_byte_offset\":10,\"end_ordinal_exclusive\":1}}}}}}\n{{\"ordinal\":1,\"type\":\"event_msg\"}}\n");
        fs::write(rollout(dir.path(), THREAD), child).unwrap();
        assert_eq!(
            repair_history_index(dir.path(), false)
                .unwrap()
                .repaired_sessions,
            1
        );
    }
    #[test]
    fn unknown_trigger_is_rejected_and_busy_writer_does_not_lose_data() {
        let dir = fixture();
        let db = Connection::open(dir.path().join(DATABASE)).unwrap();
        db.execute_batch("CREATE TRIGGER unexpected AFTER DELETE ON thread_items BEGIN DELETE FROM thread_turns; END").unwrap();
        assert!(repair_history_index(dir.path(), false).is_err());
        db.execute_batch("DROP TRIGGER unexpected; BEGIN IMMEDIATE;")
            .unwrap();
        assert!(repair_history_index(dir.path(), false).is_err());
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(cursors(&db).unwrap().len(), 2);
    }
}
