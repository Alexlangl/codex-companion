use super::ProxyConfig;

/// WinHTTP reads the current user's Internet Options without changing them.
/// PAC/WPAD cannot be represented by static HTTP_PROXY variables; only manual
/// settings are imported here.
#[cfg(target_os = "windows")]
pub(super) fn read_system_proxy() -> Option<ProxyConfig> {
    use windows_sys::Win32::{
        Foundation::GlobalFree,
        Networking::WinHttp::{
            WinHttpGetIEProxyConfigForCurrentUser, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
        },
    };
    let mut config: WINHTTP_CURRENT_USER_IE_PROXY_CONFIG = unsafe { std::mem::zeroed() };
    let success = unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } != 0;
    // All returned strings belong to the caller, including the unused PAC URL.
    unsafe fn string(pointer: *const u16) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        let mut len = 0;
        while unsafe { *pointer.add(len) } != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(pointer, len)
        }))
    }
    let proxy = success
        .then(|| unsafe { string(config.lpszProxy) })
        .flatten();
    let bypass = success
        .then(|| unsafe { string(config.lpszProxyBypass) })
        .flatten();
    for pointer in [
        config.lpszProxy,
        config.lpszProxyBypass,
        config.lpszAutoConfigUrl,
    ] {
        if !pointer.is_null() {
            unsafe {
                GlobalFree(pointer.cast());
            }
        }
    }
    proxy.map(|proxy| parse_manual_proxy(&proxy, bypass.as_deref().unwrap_or_default()))
}

fn parse_manual_proxy(proxy: &str, bypass: &str) -> ProxyConfig {
    let mut config = ProxyConfig::default();
    for entry in proxy.split([';', ' ']).filter(|v| !v.is_empty()) {
        if let Some((protocol, value)) = entry.split_once('=') {
            let value = proxy_url(value);
            match protocol.to_ascii_lowercase().as_str() {
                "http" => config.http = value,
                "https" => config.https = value,
                // WinINet's legacy SOCKS entry is SOCKS4, not SOCKS5.
                _ => {}
            }
        } else {
            let value = proxy_url(entry);
            config.http = value.clone();
            config.https = value;
        }
    }
    config.exceptions = bypass
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    config.exclude_simple_hostnames = config
        .exceptions
        .iter()
        .any(|s| s.eq_ignore_ascii_case("<local>"));
    config
}

fn proxy_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let value = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let url = url::Url::parse(&value).ok()?;
    (url.has_host() && matches!(url.scheme(), "http" | "https")).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_settings_route_protocols_and_bypass_local_hosts() {
        let config = parse_manual_proxy(
            "http=one.example:8080;https=two.example:8081",
            "<local>;*.internal.example;10.0.0.0/8",
        );
        assert_eq!(
            config
                .proxy_for(&url::Url::parse("http://public.example").unwrap())
                .as_deref(),
            Some("http://one.example:8080")
        );
        assert_eq!(
            config
                .proxy_for(&url::Url::parse("https://public.example").unwrap())
                .as_deref(),
            Some("http://two.example:8081")
        );
        for url in [
            "http://printer",
            "https://api.internal.example",
            "http://10.2.3.4",
            "http://127.0.0.1:17687",
        ] {
            assert!(config.proxy_for(&url::Url::parse(url).unwrap()).is_none());
        }
        let shared = parse_manual_proxy("localhost:7890", "");
        assert_eq!(shared.http, shared.https);
        assert_eq!(shared.https.as_deref(), Some("http://localhost:7890"));
        let https_only = parse_manual_proxy("https=localhost:7890", "");
        assert!(https_only.http.is_none());
    }
}
