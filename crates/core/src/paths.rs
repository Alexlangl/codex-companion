use crate::error::{CompanionError, Result};
use std::path::PathBuf;

pub fn default_data_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(CompanionError::HomeDirUnavailable)?;
    Ok(home.join(".codex-companion"))
}

pub fn default_config_path() -> Result<PathBuf> {
    Ok(default_data_dir()?.join("config.json"))
}

pub fn default_codex_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(CompanionError::HomeDirUnavailable)?;
    Ok(home.join(".codex"))
}
