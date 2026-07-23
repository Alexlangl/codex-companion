use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    append_diagnostic_log, clear_diagnostic_logs, diagnostic_info, DiagnosticInfo, Result,
};
use std::process::Command;

impl CompanionDaemon {
    pub fn diagnostic_info(&self) -> DiagnosticInfo {
        diagnostic_info(&self.store.data_dir())
    }

    pub fn clear_diagnostic_logs(&self) -> Result<usize> {
        clear_diagnostic_logs(&self.store.data_dir())
    }

    pub fn report_frontend_error(
        &self,
        message: &str,
        stack: Option<&str>,
        component_stack: Option<&str>,
    ) -> Result<()> {
        let details = [
            Some(message.trim()),
            stack.map(str::trim).filter(|value| !value.is_empty()),
            component_stack
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        let details = details.chars().take(32_768).collect::<String>();
        append_diagnostic_log(&self.store.data_dir(), "error", "frontend", &details)
    }

    pub fn open_diagnostic_directory(&self) -> Result<bool> {
        let directory = self.diagnostic_info().log_directory;
        std::fs::create_dir_all(&directory)
            .map_err(|source| codex_companion_core::CompanionError::io(&directory, source))?;
        Ok(open_directory(&directory))
    }
}

#[cfg(target_os = "macos")]
fn open_directory(path: &std::path::Path) -> bool {
    Command::new("open")
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "windows")]
fn open_directory(path: &std::path::Path) -> bool {
    Command::new("explorer")
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn open_directory(path: &std::path::Path) -> bool {
    Command::new("xdg-open")
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}
