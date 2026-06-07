use crate::constants::DEFAULT_GROUP_ID;
use crate::error::{CompanionError, Result};
use crate::paths::default_config_path;
use crate::types::{CompanionConfig, ProviderGroup};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
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

    pub fn save(&self, config: &CompanionConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| CompanionError::io(parent, source))?;
        }
        let text = serde_json::to_string_pretty(config)
            .map_err(|source| CompanionError::json(&self.path, source))?;
        fs::write(&self.path, format!("{text}\n"))
            .map_err(|source| CompanionError::io(&self.path, source))
    }

    pub fn update<F, T>(&self, update: F) -> Result<T>
    where
        F: FnOnce(&mut CompanionConfig) -> Result<T>,
    {
        let mut config = self.load()?;
        let output = update(&mut config)?;
        self.save(&config)?;
        Ok(output)
    }
}

pub fn ensure_default_group(config: &mut CompanionConfig) {
    config
        .groups
        .entry(DEFAULT_GROUP_ID.to_string())
        .or_insert_with(ProviderGroup::default_group);
}
