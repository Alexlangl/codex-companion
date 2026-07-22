use crate::runtime::CompanionDaemon;
use codex_companion_core::{Result, SessionPage};
use codex_companion_state::list_sessions_cached;
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn session_page(
        &self,
        codex_dir: PathBuf,
        query: Option<&str>,
        limit: usize,
        rebuild: bool,
    ) -> Result<SessionPage> {
        list_sessions_cached(
            codex_dir,
            self.store.data_dir().join("cache"),
            query,
            limit,
            rebuild,
        )
    }
}
