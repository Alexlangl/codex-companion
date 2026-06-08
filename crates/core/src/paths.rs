use crate::error::{CompanionError, Result};
use std::env;
use std::path::PathBuf;

pub fn default_data_dir() -> Result<PathBuf> {
    if let Some(path) = env_path("CODEX_COMPANION_HOME") {
        return Ok(path);
    }
    let home = dirs::home_dir().ok_or(CompanionError::HomeDirUnavailable)?;
    Ok(home.join(".codex-companion"))
}

pub fn default_config_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("config.json"))
}

pub fn default_codex_dir() -> Result<PathBuf> {
    if let Some(path) = env_path("CODEX_COMPANION_CODEX_DIR") {
        return Ok(path);
    }
    let home = dirs::home_dir().ok_or(CompanionError::HomeDirUnavailable)?;
    Ok(home.join(".codex"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
