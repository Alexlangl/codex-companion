use std::sync::LazyLock;

const FALLBACK_VERSION: &str = "26.820.60940";

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.contains('.')
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
}

fn system_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

static INSTALLED_VERSION: LazyLock<Option<String>> = LazyLock::new(|| {
    #[cfg(target_os = "macos")]
    return system_output(
        "/usr/bin/plutil",
        &[
            "-extract",
            "CFBundleShortVersionString",
            "raw",
            "-o",
            "-",
            "/Applications/Codex.app/Contents/Info.plist",
        ],
    )
    .filter(|v| valid_version(v));
    #[cfg(not(target_os = "macos"))]
    None
});

static OS_VERSION: LazyLock<String> = LazyLock::new(|| {
    if cfg!(target_os = "macos") {
        system_output("/usr/bin/sw_vers", &["-productVersion"])
    } else if cfg!(unix) {
        system_output("uname", &["-r"])
    } else {
        None
    }
    .unwrap_or_else(|| "unknown".into())
});

pub(crate) fn quota_user_agent(configured: Option<&str>) -> String {
    let override_version = std::env::var("CODEX_COMPANION_CODEX_VERSION").ok();
    let version = configured
        .filter(|v| valid_version(v))
        .or_else(|| override_version.as_deref().filter(|v| valid_version(v)))
        .or(INSTALLED_VERSION.as_deref())
        .unwrap_or(FALLBACK_VERSION);
    let os = match std::env::consts::OS {
        "macos" => "Mac OS",
        "windows" => "Windows",
        "linux" => "Linux",
        value => value,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        value => value,
    };
    format!("Codex Desktop/{version} ({os} {}; {arch})", *OS_VERSION)
}

/// Fetch only the public version field. No credentials or local identifiers
/// are included in this request. Last known good data survives offline periods.
pub(crate) async fn refresh_remote_version(
    configured: Option<&str>,
    directory: &std::path::Path,
) -> Option<String> {
    static FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = FETCH.lock().await;
    if let Some(version) = configured.filter(|v| valid_version(v)) {
        return Some(version.to_owned());
    }
    if let Ok(version) = std::env::var("CODEX_COMPANION_CODEX_VERSION") {
        if valid_version(&version) {
            return Some(version);
        }
    }
    let path = directory.join("codex-version-cache.json");
    let cached = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let last = cached
        .as_ref()
        .and_then(|v| v.get("version"))
        .and_then(serde_json::Value::as_str)
        .filter(|v| valid_version(v))
        .map(str::to_owned);
    let fresh = cached
        .as_ref()
        .and_then(|v| v.get("checkedAt"))
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|at| (0..3600).contains(&(chrono::Utc::now().timestamp() - at)));
    if fresh {
        return last;
    }
    #[cfg(test)]
    return last;
    #[cfg(not(test))]
    {
        let result = fetch_version(
            "https://raw.githubusercontent.com/jlcodes99/cockpit-tools/main/remote-config.json",
        )
        .await
        .or(last);
        let cache =
            serde_json::json!({"version":result, "checkedAt":chrono::Utc::now().timestamp()});
        let _ =
            codex_companion_core::atomic_write_private_file(&path, cache.to_string().as_bytes());
        result
    }
}

async fn fetch_version(url: &str) -> Option<String> {
    let client = codex_companion_core::http_client_builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header("User-Agent", "Codex-Companion")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let bytes = crate::http::read_response_bytes_limited(response, 256 * 1024)
        .await
        .ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    payload
        .get("codexOauthAppVersion")
        .and_then(serde_json::Value::as_str)
        .filter(|v| valid_version(v))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn public_version_fetch_and_offline_cache_preserve_precedence() {
        use axum::{routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/version", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/version",
                    get(|headers: axum::http::HeaderMap| async move {
                        assert!(!headers.contains_key("authorization"));
                        assert!(!headers.contains_key("chatgpt-account-id"));
                        r#"{"codexOauthAppVersion":"26.900.12345"}"#
                    }),
                ),
            )
            .await
            .unwrap();
        });
        assert_eq!(fetch_version(&url).await.as_deref(), Some("26.900.12345"));
        server.abort();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("codex-version-cache.json"),
            r#"{"version":"26.901.12345","checkedAt":1}"#,
        )
        .unwrap();
        assert_eq!(
            refresh_remote_version(None, temp.path()).await.as_deref(),
            Some("26.901.12345")
        );
        assert_eq!(
            refresh_remote_version(Some("26.902.12345"), temp.path())
                .await
                .as_deref(),
            Some("26.902.12345")
        );
        assert!(!valid_version("26.1\r\nInjected: 1"));
    }
}
