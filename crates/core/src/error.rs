use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompanionError {
    #[error("could not find a home directory")]
    HomeDirUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("JSON error at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

impl CompanionError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, CompanionError>;
