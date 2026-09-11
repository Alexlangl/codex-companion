use reqwest::ClientBuilder;

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", test))]
mod windows;

const PROXY_KEYS: [(&str, &str); 3] = [
    ("http_proxy", "HTTP_PROXY"),
    ("https_proxy", "HTTPS_PROXY"),
    ("all_proxy", "ALL_PROXY"),
];

#[derive(Clone, Default)]
struct ProxyConfig {
    http: Option<String>,
    https: Option<String>,
    all: Option<String>,
    exceptions: Vec<String>,
    exclude_simple_hostnames: bool,
}

/// HTTP and Responses WebSocket traffic share proxy discovery and bypass rules.
pub fn http_client_builder() -> ClientBuilder {
    apply_proxy_config(reqwest::Client::builder(), effective_proxy_config())
}

/// Explicitly pass proxy settings when launching clients, including via OS app
/// launchers which may not inherit the Companion process environment.
pub fn desktop_proxy_environment() -> Vec<(String, String)> {
    effective_proxy_config().environment()
}

fn effective_proxy_config() -> ProxyConfig {
    resolve_proxy_config(|key| std::env::var(key).ok(), read_system_proxy)
}

fn resolve_proxy_config(
    env: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> ProxyConfig,
) -> ProxyConfig {
    let values = PROXY_KEYS.map(|(lower, upper)| {
        env(lower)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| env(upper).filter(|v| !v.trim().is_empty()))
    });
    let mut config = if values.iter().any(Option::is_some) {
        let [http, https, all] = values;
        ProxyConfig {
            http,
            https,
            all,
            ..Default::default()
        }
    } else {
        system()
    };
    if let Some(no_proxy) = env("no_proxy").or_else(|| env("NO_PROXY")) {
        config.exceptions.extend(
            no_proxy
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
        );
    }
    config
}

fn read_system_proxy() -> ProxyConfig {
    #[cfg(target_os = "macos")]
    {
        macos::read_system_proxy().unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        windows::read_system_proxy().unwrap_or_default()
    }
    #[cfg(target_os = "linux")]
    {
        linux::read_system_proxy().unwrap_or_default()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    ProxyConfig::default()
}

impl ProxyConfig {
    fn proxy_for(&self, url: &url::Url) -> Option<String> {
        if should_bypass_proxy(
            url.host_str(),
            self.exclude_simple_hostnames,
            &self.exceptions,
        ) {
            return None;
        }
        match url.scheme() {
            "http" | "ws" => self.http.as_ref().or(self.all.as_ref()).cloned(),
            "https" | "wss" => self.https.as_ref().or(self.all.as_ref()).cloned(),
            _ => None,
        }
    }

    fn environment(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for ((lower, upper), value) in PROXY_KEYS.iter().zip([&self.http, &self.https, &self.all]) {
            if let Some(value) = value {
                result.push(((*upper).to_string(), value.clone()));
                result.push(((*lower).to_string(), value.clone()));
            }
        }
        // Also protect loopback when there is no active proxy: a login shell
        // may set one later. CIDR covers the full loopback range.
        let mut exceptions = vec![
            "localhost".to_string(),
            "127.0.0.0/8".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        exceptions.extend(
            self.exceptions
                .iter()
                .filter(|v| !v.eq_ignore_ascii_case("<local>"))
                .map(|v| {
                    v.strip_prefix("*.")
                        .map(|domain| format!(".{domain}"))
                        .unwrap_or_else(|| v.clone())
                }),
        );
        let bypass = exceptions.join(",");
        result.push(("NO_PROXY".to_string(), bypass.clone()));
        result.push(("no_proxy".to_string(), bypass));
        result
    }
}

fn apply_proxy_config(builder: ClientBuilder, config: ProxyConfig) -> ClientBuilder {
    // Disable reqwest's separate automatic detection so explicit environment
    // proxies get the same loopback protection as platform-discovered proxies.
    builder
        .no_proxy()
        .proxy(reqwest::Proxy::custom(move |url| config.proxy_for(url)))
}

#[cfg(test)]
mod tests;

fn should_bypass_proxy(
    host: Option<&str>,
    exclude_simple_hostnames: bool,
    exceptions: &[String],
) -> bool {
    let Some(host) = host.map(|host| {
        host.trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase()
    }) else {
        return true;
    };
    if host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
        || (exclude_simple_hostnames && is_simple_hostname(Some(&host)))
    {
        return true;
    }
    exceptions
        .iter()
        .any(|exception| exception_matches_host(exception, &host))
}

fn exception_matches_host(exception: &str, host: &str) -> bool {
    let exception = exception.trim().to_ascii_lowercase();
    if exception.is_empty() {
        return false;
    }
    if exception == "*" {
        return true;
    }
    if exception == "<local>" {
        return is_simple_hostname(Some(host));
    }
    if let Some((network, prefix)) = exception.split_once('/') {
        return cidr_contains(network, prefix, host);
    }
    let domain = exception
        .strip_prefix("*.")
        .or_else(|| exception.strip_prefix('.'))
        .unwrap_or(&exception);
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn cidr_contains(network: &str, prefix: &str, host: &str) -> bool {
    let (Ok(network), Ok(address), Ok(prefix)) = (
        network.parse::<std::net::IpAddr>(),
        host.parse::<std::net::IpAddr>(),
        prefix.parse::<u32>(),
    ) else {
        return false;
    };
    match (network, address) {
        (std::net::IpAddr::V4(network), std::net::IpAddr::V4(address)) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(network) & mask == u32::from(address) & mask
        }
        (std::net::IpAddr::V6(network), std::net::IpAddr::V6(address)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(network) & mask == u128::from(address) & mask
        }
        _ => false,
    }
}

fn is_simple_hostname(host: Option<&str>) -> bool {
    host.is_some_and(|host| !host.contains('.') && !host.contains(':'))
}
