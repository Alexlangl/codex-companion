use crate::constants::DEFAULT_GROUP_ID;
use crate::error::{CompanionError, Result};
use crate::paths::default_config_path;
use crate::types::{CompanionConfig, ProviderGroup};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::Builder;

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self> {
        Ok(Self {
            path: default_config_path()?,
        })
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn data_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn load(&self) -> Result<CompanionConfig> {
        self.load_unlocked()
    }

    pub fn save(&self, config: &CompanionConfig) -> Result<()> {
        let _guard = self.lock_exclusive()?;
        self.save_unlocked(config)
    }

    pub fn update<F, T>(&self, update: F) -> Result<T>
    where
        F: FnOnce(&mut CompanionConfig) -> Result<T>,
    {
        let _guard = self.lock_exclusive()?;
        let mut config = self.load_unlocked()?;
        let output = update(&mut config)?;
        self.save_unlocked(&config)?;
        Ok(output)
    }

    fn load_unlocked(&self) -> Result<CompanionConfig> {
        if !self.path.exists() {
            return Ok(CompanionConfig::default());
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|source| CompanionError::io(&self.path, source))?;
        let mut config: CompanionConfig = serde_json::from_str(&text)
            .map_err(|source| CompanionError::json(&self.path, source))?;
        ensure_default_group(&mut config);
        Ok(config)
    }

    fn save_unlocked(&self, config: &CompanionConfig) -> Result<()> {
        let parent = self.ensure_parent_dir()?;
        let text = serde_json::to_string_pretty(config)
            .map_err(|source| CompanionError::json(&self.path, source))?;
        let file_name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config.json");
        let mut temporary = Builder::new()
            .prefix(&format!(".{file_name}."))
            .suffix(".tmp")
            .tempfile_in(&parent)
            .map_err(|source| CompanionError::io(&parent, source))?;

        if let Ok(metadata) = fs::metadata(&self.path) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())
                .map_err(|source| CompanionError::io(temporary.path(), source))?;
        }
        temporary
            .write_all(format!("{text}\n").as_bytes())
            .map_err(|source| CompanionError::io(temporary.path(), source))?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|source| CompanionError::io(temporary.path(), source))?;
        temporary
            .persist(&self.path)
            .map_err(|error| CompanionError::io(&self.path, error.error))?;
        sync_parent_dir(&parent)
    }

    fn lock_exclusive(&self) -> Result<File> {
        let parent = self.ensure_parent_dir()?;
        let file_name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config.json");
        let lock_path = parent.join(format!(".{file_name}.lock"));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|source| CompanionError::io(&lock_path, source))?;
        lock_file
            .lock()
            .map_err(|source| CompanionError::io(&lock_path, source))?;
        Ok(lock_file)
    }

    fn ensure_parent_dir(&self) -> Result<PathBuf> {
        let parent = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        fs::create_dir_all(&parent).map_err(|source| CompanionError::io(&parent, source))?;
        Ok(parent)
    }
}

#[cfg(unix)]
fn sync_parent_dir(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CompanionError::io(parent, source))
}

#[cfg(not(unix))]
fn sync_parent_dir(_parent: &Path) -> Result<()> {
    Ok(())
}

pub fn ensure_default_group(config: &mut CompanionConfig) {
    config
        .groups
        .entry(DEFAULT_GROUP_ID.to_string())
        .or_insert_with(ProviderGroup::default_group);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GroupPolicy;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    const CHILD_MODE_ENV: &str = "CODEX_COMPANION_CONFIG_STORE_CHILD";
    const CONFIG_PATH_ENV: &str = "CODEX_COMPANION_CONFIG_STORE_PATH";
    const MARKER_PATH_ENV: &str = "CODEX_COMPANION_CONFIG_STORE_MARKER";

    fn group(id: &str) -> ProviderGroup {
        ProviderGroup {
            id: id.to_string(),
            name: id.to_string(),
            policy: GroupPolicy::PriorityFallback,
            provider_order: Vec::new(),
            provider_weights: Default::default(),
            fallback_enabled: true,
            priority_failback_interval_seconds: 0,
            priority_failback_revision: 0,
            priority_failback_target_provider_id: None,
        }
    }

    #[test]
    fn separate_store_instances_serialize_updates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();

        for id in ["group-a", "group-b"] {
            let store = ConfigStore::new(path.clone());
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                store
                    .update(|config| {
                        config.groups.insert(id.to_string(), group(id));
                        thread::sleep(Duration::from_millis(50));
                        Ok(())
                    })
                    .expect("update");
            }));
        }

        barrier.wait();
        for worker in workers {
            worker.join().expect("worker");
        }
        let config = ConfigStore::new(path).load().expect("load");
        assert!(config.groups.contains_key("group-a"));
        assert!(config.groups.contains_key("group-b"));
    }

    #[test]
    fn cross_process_updates_are_serialized() {
        if let Some(mode) = std::env::var_os(CHILD_MODE_ENV) {
            run_update_child(mode.to_string_lossy().as_ref());
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let marker_path = temp.path().join("first-update-started");
        let test_name = "store::tests::cross_process_updates_are_serialized";
        let mut first = Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_MODE_ENV, "first")
            .env(CONFIG_PATH_ENV, &config_path)
            .env(MARKER_PATH_ENV, &marker_path)
            .spawn()
            .expect("spawn first child");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker_path.exists(), "first child did not enter its update");

        let second = Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_MODE_ENV, "second")
            .env(CONFIG_PATH_ENV, &config_path)
            .env(MARKER_PATH_ENV, &marker_path)
            .status()
            .expect("run second child");
        assert!(second.success());
        assert!(first.wait().expect("wait first child").success());

        let config = ConfigStore::new(config_path).load().expect("load");
        assert!(config.groups.contains_key("group-first"));
        assert!(config.groups.contains_key("group-second"));
    }

    fn run_update_child(mode: &str) {
        let config_path = PathBuf::from(std::env::var_os(CONFIG_PATH_ENV).expect("config path"));
        let marker_path = PathBuf::from(std::env::var_os(MARKER_PATH_ENV).expect("marker path"));
        let store = ConfigStore::new(config_path);
        store
            .update(|config| {
                if mode == "first" {
                    fs::write(&marker_path, b"started").expect("write marker");
                    thread::sleep(Duration::from_millis(300));
                }
                let id = format!("group-{mode}");
                config.groups.insert(id.clone(), group(&id));
                Ok(())
            })
            .expect("child update");
    }

    #[test]
    fn readers_only_observe_complete_json_during_updates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let store = ConfigStore::new(path.clone());
        store
            .save(&CompanionConfig::default())
            .expect("seed config");
        let writer = thread::spawn(move || {
            let store = ConfigStore::new(path);
            for index in 0..100 {
                store
                    .update(|config| {
                        config
                            .groups
                            .insert(format!("group-{index}"), group(&format!("group-{index}")));
                        Ok(())
                    })
                    .expect("write config");
            }
        });

        while !writer.is_finished() {
            store.load().expect("reader must see valid JSON");
        }
        writer.join().expect("writer");
        assert_eq!(store.load().expect("final config").groups.len(), 101);
    }
}
