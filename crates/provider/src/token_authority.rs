//! Runtime ownership is discovered from processes, never inferred from a stale
//! account selection. All process/auth contents stay local and out of logs.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct RuntimeSnapshot {
    pub sources: Vec<PathBuf>,
    pub live: Vec<Value>,
    pub uncertain: bool,
}

pub(crate) fn identity(value: &Value) -> Option<String> {
    let normalized = crate::import::extract_codex_oauth_auth(value)?;
    let user = normalized.pointer("/tokens/user_id")?.as_str()?.trim();
    let account = normalized.pointer("/tokens/account_id")?.as_str()?.trim();
    if user.is_empty() || account.is_empty() {
        return None;
    }
    Some(format!("oauth:{}:{user}:{account}", user.len()))
}

pub(crate) fn same_account(left: &Value, right: &Value) -> bool {
    identity(left)
        .zip(identity(right))
        .is_some_and(|(a, b)| a == b)
}

pub(crate) fn coordination_dir(auth_path: &Path) -> PathBuf {
    #[cfg(test)]
    return auth_path.parent().unwrap().join("authority-test");
    #[cfg(not(test))]
    {
        let _ = auth_path;
        codex_companion_core::account_coordination_dir()
    }
}

pub(crate) fn authority_path(auth_path: &Path, value: &Value) -> PathBuf {
    let key = identity(value).unwrap_or_else(|| {
        let token = value
            .pointer("/tokens/refresh_token")
            .or_else(|| value.get("refresh_token"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if token.is_empty() {
            auth_path.to_string_lossy().into_owned()
        } else {
            format!("rt:{token}")
        }
    });
    coordination_dir(auth_path)
        .join("tokens")
        .join(format!("{:x}.json", Sha256::digest(key.as_bytes())))
}

impl RuntimeSnapshot {
    pub fn capture(auth: &Value) -> Self {
        // Tests use explicitly supplied fixtures and never inspect real accounts.
        #[cfg(test)]
        let discovered: Result<Vec<PathBuf>, ()> = Ok(Vec::new());
        #[cfg(not(test))]
        let discovered = discover_runtime_homes();
        let mut snapshot = Self {
            uncertain: discovered.is_err(),
            ..Self::default()
        };
        let mut sources = BTreeSet::new();
        if let Some(source) = auth
            .get("companion_local_auth_source")
            .and_then(Value::as_str)
        {
            sources.insert(PathBuf::from(source));
        }
        #[cfg(not(test))]
        {
            if let Some(home) = dirs::home_dir() {
                sources.insert(home.join(".codex/auth.json"));
            }
            if let Ok(home) = codex_companion_core::default_codex_dir() {
                sources.insert(home.join("auth.json"));
            }
            if let Some(home) = std::env::var_os("CODEX_HOME") {
                sources.insert(PathBuf::from(home).join("auth.json"));
            }
        }
        for home in discovered.unwrap_or_default() {
            let path = home.join("auth.json");
            sources.insert(path.clone());
            match read_runtime_auth(&home) {
                Some(value) => snapshot.live.push(value),
                None => snapshot.uncertain = true,
            }
        }
        snapshot.sources = sources.into_iter().collect();
        snapshot
    }

    pub fn owns(&self, auth: &Value) -> bool {
        self.uncertain
            || self.live.iter().any(|candidate| {
                same_account(auth, candidate)
                    || (identity(auth).is_none()
                        && crate::import::extract_codex_oauth_auth(candidate).is_some())
            })
    }
}

pub(crate) fn read_runtime_auth(home: &Path) -> Option<Value> {
    let file = std::fs::read(home.join("auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let keyring = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            doc.get("cli_auth_credentials_store")
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned)
        });
    if file.is_some() && !matches!(keyring.as_deref(), Some("keyring" | "auto")) {
        return file;
    }
    #[cfg(all(target_os = "macos", not(test)))]
    {
        let path = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        let digest = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let output = std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                "Codex Auth",
                "-a",
                &format!("cli|{}", &digest[..16]),
                "-w",
            ])
            .output()
            .ok()?;
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).ok();
        }
    }
    if keyring.as_deref() == Some("keyring") {
        None
    } else {
        file
    }
}

#[cfg(all(not(test), unix))]
fn discover_runtime_homes() -> Result<Vec<PathBuf>, ()> {
    let listing = std::process::Command::new("ps")
        .args(["-axo", "pid=,comm="])
        .output()
        .map_err(|_| ())?;
    if !listing.status.success() {
        return Err(());
    }
    let default = dirs::home_dir().ok_or(())?.join(".codex");
    let mut homes = BTreeSet::new();
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let line = line.trim();
        let Some(split) = line.find(char::is_whitespace) else {
            continue;
        };
        let pid = line[..split].parse::<u32>().map_err(|_| ())?;
        let executable = line[split..].trim();
        let name = Path::new(executable)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "codex" | "chatgpt" | "codex-app-server" | "codex app-server"
        ) {
            continue;
        }
        #[cfg(target_os = "linux")]
        let environment = std::fs::read(format!("/proc/{pid}/environ")).map_err(|_| ())?;
        #[cfg(not(target_os = "linux"))]
        let environment = {
            let output = std::process::Command::new("ps")
                .args(["eww", "-p", &pid.to_string(), "-o", "command="])
                .output()
                .map_err(|_| ())?;
            if !output.status.success() {
                return Err(());
            }
            output.stdout
        };
        let env = String::from_utf8_lossy(&environment);
        homes.insert(extract_home(&env).unwrap_or_else(|| default.clone()));
    }
    Ok(homes.into_iter().collect())
}

#[cfg(all(not(test), windows))]
fn discover_runtime_homes() -> Result<Vec<PathBuf>, ()> {
    // CIM cannot reliably read another process's environment. Discover known
    // command-line homes, and defer refresh if an owner's home is ambiguous.
    let output = std::process::Command::new("powershell").args(["-NoProfile", "-Command",
        "@(Get-CimInstance Win32_Process | Where-Object { $_.Name -in @('Codex.exe','codex-app-server.exe','ChatGPT.exe') } | Select-Object CommandLine) | ConvertTo-Json -Compress"])
        .output().map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
    let entries = match value {
        Value::Array(entries) => entries,
        Value::Object(_) => vec![value],
        _ => return Err(()),
    };
    entries
        .iter()
        .map(|entry| {
            entry
                .get("CommandLine")
                .and_then(Value::as_str)
                .and_then(extract_home)
                .ok_or(())
        })
        .collect()
}

fn extract_home(environment: &str) -> Option<PathBuf> {
    let value = environment.split("CODEX_HOME=").nth(1)?;
    let end = if environment.contains('\0') {
        value.find('\0').unwrap_or(value.len())
    } else {
        // ps flattens environment values, including spaces in directory names.
        value
            .char_indices()
            .filter(|(_, c)| c.is_whitespace())
            .find_map(|(i, _)| {
                let next = value[i..].trim_start().split_whitespace().next()?;
                let key = next.split_once('=')?.0;
                (!key.is_empty() && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'))
                    .then_some(i)
            })
            .unwrap_or(value.len())
    };
    let path = value[..end].trim().trim_matches('"');
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_home_preserves_spaces_and_stops_at_next_variable() {
        assert_eq!(
            extract_home("codex CODEX_HOME=/tmp/My Codex HOME=/tmp/user"),
            Some(PathBuf::from("/tmp/My Codex"))
        );
        assert_eq!(
            extract_home("CODEX_HOME=/tmp/one\0OTHER=x\0"),
            Some(PathBuf::from("/tmp/one"))
        );
        assert!(RuntimeSnapshot {
            uncertain: true,
            ..Default::default()
        }
        .owns(&Value::Null));
    }
}
