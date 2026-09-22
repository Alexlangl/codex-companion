//! Provider migrations keep an undo journal for metadata lines, not copies of conversations.
//! The journal is durable before the atomically replaced session becomes visible.
use super::*;
use std::io::{BufWriter, Read, Write};

pub(super) const PATCH_DIR: &str = "history-patches-v1";

#[derive(Serialize, Deserialize)]
struct LineChange {
    line: usize,
    before: String,
    after: String,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    changes: Vec<LineChange>,
}

fn temporary_for(path: &Path) -> Result<tempfile::NamedTempFile> {
    tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
        .map_err(|source| CompanionError::io(path, source))
}

fn commit(temporary: tempfile::NamedTempFile, path: &Path, initial: &fs::Metadata) -> Result<()> {
    let current = fs::metadata(path).map_err(|source| CompanionError::io(path, source))?;
    if current.len() != initial.len() || current.modified().ok() != initial.modified().ok() {
        return Err(CompanionError::InvalidConfig(format!(
            "会话在修复期间发生变化，未覆盖: {}",
            path.display()
        )));
    }
    temporary
        .as_file()
        .set_permissions(initial.permissions())
        .map_err(|source| CompanionError::io(path, source))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| CompanionError::io(path, source))?;
    temporary
        .persist(path)
        .map_err(|error| CompanionError::io(path, error.error))?;
    #[cfg(unix)]
    fs::File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CompanionError::io(path, source))?;
    Ok(())
}

pub(super) fn rewrite(
    path: &Path,
    source_ids: &BTreeSet<String>,
    target: &str,
    backup_root: &Path,
    codex_dir: &Path,
) -> Result<usize> {
    let input = fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
    let initial = input
        .metadata()
        .map_err(|source| CompanionError::io(path, source))?;
    let mut reader = BufReader::new(input);
    let mut temporary = None;
    let mut writer: Option<BufWriter<fs::File>> = None;
    let mut offset = 0u64;
    let mut changes = Vec::new();
    let mut line = String::new();
    let mut index = 0;
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(|source| CompanionError::io(path, source))?
            == 0
        {
            break;
        }
        let ending = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        if let Some(next) = rewrite_session_meta_provider_line(&line, source_ids, target)? {
            if writer.is_none() {
                let file = temporary_for(path)?;
                let mut output = BufWriter::new(
                    file.as_file()
                        .try_clone()
                        .map_err(|source| CompanionError::io(path, source))?,
                );
                let prefix =
                    fs::File::open(path).map_err(|source| CompanionError::io(path, source))?;
                std::io::copy(&mut prefix.take(offset), &mut output)
                    .map_err(|source| CompanionError::io(path, source))?;
                writer = Some(output);
                temporary = Some(file);
            }
            let after = format!("{next}{ending}");
            writer
                .as_mut()
                .expect("initialized writer")
                .write_all(after.as_bytes())
                .map_err(|source| CompanionError::io(path, source))?;
            changes.push(LineChange {
                line: index,
                before: line.clone(),
                after,
            });
        } else if let Some(writer) = writer.as_mut() {
            writer
                .write_all(line.as_bytes())
                .map_err(|source| CompanionError::io(path, source))?;
        }
        offset += line.len() as u64;
        index += 1;
    }
    if let Some(writer) = writer.as_mut() {
        writer
            .flush()
            .map_err(|source| CompanionError::io(path, source))?;
    }
    drop(writer);
    drop(reader);
    let Some(temporary) = temporary else {
        return Ok(0);
    };
    let count = changes.len();
    let relative = path
        .strip_prefix(codex_dir)
        .map_err(|error| CompanionError::InvalidConfig(error.to_string()))?;
    let mut journal_path = backup_root.join(PATCH_DIR).join(relative);
    journal_path.set_extension("json");
    let bytes = serde_json::to_vec(&Journal {
        version: 1,
        changes,
    })
    .map_err(|source| CompanionError::json(&journal_path, source))?;
    atomic_write_private_file(&journal_path, &bytes)?;
    commit(temporary, path, &initial)?;
    Ok(count)
}

pub(super) fn restore(backup_root: &Path, codex_dir: &Path) -> Result<()> {
    let root = backup_root.join(PATCH_DIR);
    if !root.exists() {
        return Ok(()); // Older snapshots contain complete files instead.
    }
    for entry in WalkDir::new(&root) {
        let entry = entry.map_err(|error| CompanionError::InvalidConfig(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let journal_path = entry.path();
        let bytes =
            fs::read(journal_path).map_err(|source| CompanionError::io(journal_path, source))?;
        let journal: Journal = serde_json::from_slice(&bytes)
            .map_err(|source| CompanionError::json(journal_path, source))?;
        if journal.version != 1
            || journal
                .changes
                .windows(2)
                .any(|pair| pair[0].line >= pair[1].line)
        {
            return Err(CompanionError::InvalidConfig(
                "无法识别会话恢复日志".to_string(),
            ));
        }
        let relative = journal_path
            .strip_prefix(&root)
            .map_err(|error| CompanionError::InvalidConfig(error.to_string()))?;
        let mut path = codex_dir.join(relative);
        path.set_extension("jsonl");
        let input = fs::File::open(&path).map_err(|source| CompanionError::io(&path, source))?;
        let initial = input
            .metadata()
            .map_err(|source| CompanionError::io(&path, source))?;
        let mut reader = BufReader::new(input);
        let mut temporary = temporary_for(&path)?;
        let mut writer = BufWriter::new(temporary.as_file_mut());
        let mut changes = journal.changes.iter().peekable();
        let mut line = String::new();
        let mut index = 0;
        loop {
            line.clear();
            if reader
                .read_line(&mut line)
                .map_err(|source| CompanionError::io(&path, source))?
                == 0
            {
                break;
            }
            let text = if changes.peek().is_some_and(|change| change.line == index) {
                let change = changes.next().expect("peeked change");
                // Accept the original too: a failure may precede the atomic replacement.
                if line != change.before && line != change.after {
                    return Err(CompanionError::InvalidConfig(format!(
                        "会话元数据已变化，未覆盖: {}:{}",
                        path.display(),
                        index + 1
                    )));
                }
                &change.before
            } else {
                &line
            };
            writer
                .write_all(text.as_bytes())
                .map_err(|source| CompanionError::io(&path, source))?;
            index += 1;
        }
        if changes.next().is_some() {
            return Err(CompanionError::InvalidConfig(format!(
                "会话不完整，未覆盖: {}",
                path.display()
            )));
        }
        writer
            .flush()
            .map_err(|source| CompanionError::io(&path, source))?;
        drop(writer);
        drop(reader);
        commit(temporary, &path, &initial)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str =
        "{ \"type\": \"session_meta\", \"payload\": {\"model_provider\":\"openai\"} }";

    #[test]
    fn journal_size_is_independent_of_conversation_size_and_rollback_is_exact() {
        for ending in ["\n", "\r\n", ""] {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("sessions/session.jsonl");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            // Include a large tool output, invalid JSON, and a metadata-like non-session event.
            let prefix = format!("{}\r\nnot-json\n{{\"type\":\"event_msg\",\"payload\":{{\"model_provider\":\"openai\",\"text\":\"session_meta\"}}}}\n", "x".repeat(2_000_000));
            let original = format!("{prefix}{META}{ending}");
            fs::write(&file, &original).unwrap();
            let backup = temp.path().join("backup");
            let ids = BTreeSet::from(["openai".to_string()]);
            assert_eq!(
                rewrite(&file, &ids, "relay", &backup, temp.path()).unwrap(),
                1
            );
            let journal = backup.join(PATCH_DIR).join("sessions/session.json");
            assert!(fs::metadata(journal).unwrap().len() < 1024);
            assert!(!backup.join("sessions/session.jsonl").exists());
            let migrated = fs::read_to_string(&file).unwrap();
            assert!(migrated.starts_with(&prefix));
            assert_eq!(migrated.ends_with('\n'), !ending.is_empty());
            restore_repair_backup(&backup, temp.path()).unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), original);
            assert!(!temp.path().join(PATCH_DIR).exists());
        }
    }

    #[test]
    fn later_failure_rolls_back_journal_and_full_file_without_losing_appended_events() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sessions/session.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let original = format!("{META}\nbody\n");
        fs::write(&file, &original).unwrap();
        let plugin = temp.path().join("plugin.json");
        fs::write(&plugin, "original").unwrap();
        let ids = BTreeSet::from(["openai".to_string()]);
        let result: Result<(Option<PathBuf>, ())> = run_repair_transaction(temp.path(), |root| {
            let backup = ensure_repair_backup_root(root, temp.path())?;
            rewrite(&file, &ids, "relay", backup, temp.path())?;
            backup_file(&plugin, backup, temp.path())?;
            fs::write(&plugin, "changed").unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap()
                .write_all(b"new event\n")
                .unwrap();
            Err(CompanionError::InvalidConfig("later failure".into()))
        });
        assert!(result.unwrap_err().to_string().contains("已从"));
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            format!("{original}new event\n")
        );
        assert_eq!(fs::read_to_string(plugin).unwrap(), "original");
    }

    #[test]
    fn conflicting_or_truncated_metadata_is_not_overwritten() {
        for conflict in ["other metadata\n", ""] {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("session.jsonl");
            fs::write(&file, format!("{META}\n")).unwrap();
            let backup = temp.path().join("backup");
            rewrite(
                &file,
                &BTreeSet::from(["openai".into()]),
                "relay",
                &backup,
                temp.path(),
            )
            .unwrap();
            fs::write(&file, conflict).unwrap();
            assert!(restore(&backup, temp.path()).is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), conflict);
        }
    }

    #[test]
    fn repeated_migration_is_a_noop() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("session.jsonl");
        fs::write(&file, META).unwrap();
        let backup = temp.path().join("backup");
        let ids = BTreeSet::from(["openai".into()]);
        rewrite(&file, &ids, "relay", &backup, temp.path()).unwrap();
        let second_backup = temp.path().join("second-backup");
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        assert_eq!(
            rewrite(&file, &ids, "relay", &second_backup, temp.path()).unwrap(),
            0
        );
        assert!(!second_backup.exists());
        assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
        // A journal persisted before a failed replacement also safely restores an unchanged file.
        fs::write(&file, META).unwrap();
        restore(&backup, temp.path()).unwrap();
        assert_eq!(fs::read_to_string(file).unwrap(), META);
    }

    #[test]
    fn atomic_commit_rejects_concurrent_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(&path, "before").unwrap();
        let initial = fs::metadata(&path).unwrap();
        let mut replacement = temporary_for(&path).unwrap();
        replacement.write_all(b"replacement").unwrap();
        fs::write(&path, "concurrent append").unwrap();
        assert!(commit(replacement, &path, &initial).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "concurrent append");
    }

    #[test]
    fn namespace_switches_preserve_original_usage_provider() {
        let original = r#"{"type":"session_meta","payload":{"model_provider":"provider-a"}}"#;
        let first = rewrite_session_meta_provider_line(
            original,
            &BTreeSet::from(["provider-a".into()]),
            "codex-companion",
        )
        .unwrap()
        .unwrap();
        let second = rewrite_session_meta_provider_line(
            &first,
            &BTreeSet::from(["codex-companion".into()]),
            "provider-b",
        )
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(value["payload"]["model_provider"], "provider-b");
        assert_eq!(value["payload"]["companion_usage_provider"], "provider-a");
    }

    #[test]
    fn failed_rollback_snapshot_is_not_pruned_by_later_successes() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("session.jsonl");
        fs::write(&file, format!("{META}\n")).unwrap();
        let result: Result<(Option<PathBuf>, ())> = run_repair_transaction(temp.path(), |root| {
            let backup = ensure_repair_backup_root(root, temp.path())?;
            rewrite(
                &file,
                &BTreeSet::from(["openai".into()]),
                "relay",
                backup,
                temp.path(),
            )?;
            fs::write(&file, "externally changed metadata\n").unwrap();
            Err(CompanionError::InvalidConfig("later failure".into()))
        });
        assert!(result.unwrap_err().to_string().contains("自动回滚也失败"));
        let parent = repair_backup_parent(temp.path());
        let failed = fs::read_dir(&parent)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::create_dir(parent.join("999-newer")).unwrap();
        cleanup_repair_backups_with_limit(temp.path(), 1, 1).unwrap();
        assert!(failed.join(REPAIR_ROLLBACK_FAILED_MARKER).exists());
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "externally changed metadata\n"
        );
    }
}
