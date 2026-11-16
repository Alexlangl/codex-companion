use codex_companion_core::{CompanionError, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

pub(crate) fn compressed(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".jsonl.zst")
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path).map_err(|e| CompanionError::io(path, e))?;
    if compressed(path) {
        let decoder =
            zstd::stream::read::Decoder::new(file).map_err(|e| CompanionError::io(path, e))?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}
