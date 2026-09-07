use super::ProxyConfig;

/// Linux has no single system proxy API. GNOME-family desktops expose manual
/// proxy settings through GSettings; other environments use proxy variables.
#[cfg(target_os = "linux")]
pub(super) fn read_system_proxy() -> Option<ProxyConfig> {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !["gnome", "unity", "cinnamon", "budgie"]
        .iter()
        .any(|name| desktop.contains(name))
    {
        return None;
    }
    // Write to a private temporary file instead of a pipe so an unexpectedly
    // verbose command cannot block while the parent waits for it to finish.
    let output = tempfile::tempfile().ok()?;
    let mut child = Command::new("gsettings")
        .args(["list-recursively", "org.gnome.system.proxy"])
        .stdin(Stdio::null())
        .stdout(output.try_clone().ok()?)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(750);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if !success {
        return None;
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut output = output;
    output.seek(SeekFrom::Start(0)).ok()?;
    let mut text = String::new();
    output.take(64 * 1024).read_to_string(&mut text).ok()?;
    parse_gsettings(&text)
}

fn parse_gsettings(text: &str) -> Option<ProxyConfig> {
    let settings = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            Some(((parts.next()?, parts.next()?), parts.next()?.trim()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let get = |schema, key| settings.get(&(schema, key)).copied().unwrap_or("");
    let root = "org.gnome.system.proxy";
    if unquote(get(root, "mode"))? != "manual" {
        return None;
    }
    let endpoint = |schema, scheme| -> Option<String> {
        let host = unquote(get(schema, "host"))?;
        let port = get(schema, "port").parse::<u16>().ok().filter(|v| *v > 0)?;
        if host.is_empty() {
            return None;
        }
        let mut url = url::Url::parse(&format!("{scheme}://localhost")).ok()?;
        url.set_host(Some(&host)).ok()?;
        url.set_port(Some(port)).ok()?;
        if schema == "org.gnome.system.proxy.http" && get(schema, "use-authentication") == "true" {
            url.set_username(&unquote(get(schema, "authentication-user"))?)
                .ok()?;
            url.set_password(Some(&unquote(get(schema, "authentication-password"))?))
                .ok()?;
        }
        Some(url.to_string())
    };
    let http = endpoint("org.gnome.system.proxy.http", "http");
    let same = get(root, "use-same-proxy") == "true";
    let https = if same {
        http.clone()
    } else {
        endpoint("org.gnome.system.proxy.https", "http")
    };
    let all = if same {
        http.clone()
    } else {
        endpoint("org.gnome.system.proxy.socks", "socks5h")
    };
    let exceptions = get(root, "ignore-hosts")
        .trim_start_matches("@as ")
        .trim_matches(['[', ']'])
        .split(',')
        .filter_map(|v| unquote(v.trim()))
        .collect();
    Some(ProxyConfig {
        http,
        https,
        all,
        exceptions,
        ..Default::default()
    })
}

// GSettings strings are GVariant literals. Accept ordinary quoted values and
// escaped quotes/backslashes, rejecting unknown escapes rather than changing
// a credential's meaning.
fn unquote(value: &str) -> Option<String> {
    let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut chars = inner.chars();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars.next()?;
            if !matches!(escaped, '\\' | '\'' | '"') {
                return None;
            }
            result.push(escaped);
        } else {
            result.push(ch);
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_manual_settings_share_http_proxy_and_preserve_authentication() {
        let text = "org.gnome.system.proxy mode 'manual'\norg.gnome.system.proxy use-same-proxy true\norg.gnome.system.proxy ignore-hosts ['localhost', '.internal.example']\norg.gnome.system.proxy.http host 'proxy.example'\norg.gnome.system.proxy.http port 8080\norg.gnome.system.proxy.http use-authentication true\norg.gnome.system.proxy.http authentication-user 'user@example'\norg.gnome.system.proxy.http authentication-password 'a:b'";
        let config = parse_gsettings(text).unwrap();
        assert_eq!(config.http, config.https);
        assert_eq!(
            config.https.as_deref(),
            Some("http://user%40example:a%3Ab@proxy.example:8080/")
        );
        assert!(config
            .proxy_for(&url::Url::parse("https://api.internal.example").unwrap())
            .is_none());
        assert!(parse_gsettings(&text.replace("'manual'", "'auto'")).is_none());
        assert!(parse_gsettings(&text.replace("'manual'", "'none'")).is_none());
    }
}
