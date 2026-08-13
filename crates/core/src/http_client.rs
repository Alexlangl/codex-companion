use reqwest::ClientBuilder;

/// Builds an HTTP client that honors environment proxy variables and, for a
/// sandboxed macOS GUI process, the system HTTP/HTTPS proxy configuration.
pub fn http_client_builder() -> ClientBuilder {
    let builder = reqwest::Client::builder();

    #[cfg(target_os = "macos")]
    {
        return macos::apply_system_proxy(builder);
    }

    #[cfg(not(target_os = "macos"))]
    builder
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::c_void;
    use system_configuration::core_foundation::{
        array::CFArray,
        base::{CFType, TCFType},
        boolean::CFBoolean,
        number::CFNumber,
        string::{CFString, CFStringRef},
    };
    use system_configuration::dynamic_store::SCDynamicStoreBuilder;
    use system_configuration::sys::schema_definitions::{
        kSCPropNetProxiesExceptionsList, kSCPropNetProxiesExcludeSimpleHostnames,
        kSCPropNetProxiesHTTPEnable, kSCPropNetProxiesHTTPPort, kSCPropNetProxiesHTTPProxy,
        kSCPropNetProxiesHTTPSEnable, kSCPropNetProxiesHTTPSPort, kSCPropNetProxiesHTTPSProxy,
    };

    const PROXY_ENV_KEYS: [&str; 6] = [
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
    ];

    #[derive(Debug, Default)]
    struct SystemProxyConfig {
        http: Option<String>,
        https: Option<String>,
        exceptions: Vec<String>,
        exclude_simple_hostnames: bool,
    }

    pub(super) fn apply_system_proxy(builder: ClientBuilder) -> ClientBuilder {
        if PROXY_ENV_KEYS
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
        {
            return builder;
        }

        let Some(mut config) = read_system_proxy() else {
            return builder;
        };
        if let Some(no_proxy) =
            std::env::var_os("NO_PROXY").or_else(|| std::env::var_os("no_proxy"))
        {
            config.exceptions.extend(
                no_proxy
                    .to_string_lossy()
                    .split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_string),
            );
        }
        apply_proxy_config(builder, config)
    }

    fn apply_proxy_config(builder: ClientBuilder, config: SystemProxyConfig) -> ClientBuilder {
        if config.http.is_none() && config.https.is_none() {
            return builder;
        }

        let http = config.http;
        let https = config.https;
        let exceptions = config.exceptions;
        let exclude_simple_hostnames = config.exclude_simple_hostnames;
        let proxy = reqwest::Proxy::custom(move |url| {
            if should_bypass_proxy(url.host_str(), exclude_simple_hostnames, &exceptions) {
                return None;
            }
            match url.scheme() {
                "http" => http.clone(),
                "https" => https.clone(),
                _ => None,
            }
        });
        builder.proxy(proxy)
    }

    fn read_system_proxy() -> Option<SystemProxyConfig> {
        let store = SCDynamicStoreBuilder::new("codex-companion").build()?;
        let settings = store.get_proxies()?;
        let exceptions = string_array(&settings, unsafe { kSCPropNetProxiesExceptionsList });
        let exclude_simple_hostnames = exceptions.iter().any(|entry| entry.trim() == "<local>")
            || boolean_value(&settings, unsafe {
                kSCPropNetProxiesExcludeSimpleHostnames
            });
        Some(SystemProxyConfig {
            http: proxy_url(
                &settings,
                unsafe { kSCPropNetProxiesHTTPEnable },
                unsafe { kSCPropNetProxiesHTTPProxy },
                unsafe { kSCPropNetProxiesHTTPPort },
            ),
            https: proxy_url(
                &settings,
                unsafe { kSCPropNetProxiesHTTPSEnable },
                unsafe { kSCPropNetProxiesHTTPSProxy },
                unsafe { kSCPropNetProxiesHTTPSPort },
            ),
            exceptions,
            exclude_simple_hostnames,
        })
    }

    fn proxy_url(
        settings: &system_configuration::core_foundation::dictionary::CFDictionary<
            CFString,
            CFType,
        >,
        enabled_key: CFStringRef,
        host_key: CFStringRef,
        port_key: CFStringRef,
    ) -> Option<String> {
        let enabled = settings
            .find(enabled_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32())
            == Some(1);
        if !enabled {
            return None;
        }
        let host = settings
            .find(host_key)
            .and_then(|value| value.downcast::<CFString>())?
            .to_string();
        if host.trim().is_empty() {
            return None;
        }
        let port = settings
            .find(port_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i32());
        Some(match port {
            Some(port) => format!("http://{host}:{port}"),
            None => format!("http://{host}"),
        })
    }

    fn string_array(
        settings: &system_configuration::core_foundation::dictionary::CFDictionary<
            CFString,
            CFType,
        >,
        key: CFStringRef,
    ) -> Vec<String> {
        let Some(array) = settings
            .find(key)
            .and_then(|value| value.downcast::<CFArray<*const c_void>>())
        else {
            return Vec::new();
        };
        array
            .iter()
            .filter_map(|value| {
                let value = unsafe { CFType::wrap_under_get_rule(*value) };
                value.downcast::<CFString>().map(|value| value.to_string())
            })
            .collect()
    }

    fn boolean_value(
        settings: &system_configuration::core_foundation::dictionary::CFDictionary<
            CFString,
            CFType,
        >,
        key: CFStringRef,
    ) -> bool {
        settings.find(key).is_some_and(|value| {
            value
                .downcast::<CFBoolean>()
                .map(bool::from)
                .or_else(|| {
                    value
                        .downcast::<CFNumber>()
                        .and_then(|value| value.to_i32())
                        .map(|value| value != 0)
                })
                .unwrap_or(false)
        })
    }

    fn should_bypass_proxy(
        host: Option<&str>,
        exclude_simple_hostnames: bool,
        exceptions: &[String],
    ) -> bool {
        let Some(host) = host.map(|host| host.trim_matches(['[', ']']).to_ascii_lowercase()) else {
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::TcpListener;

        #[test]
        fn bypass_matches_loopback_domains_cidrs_and_local_names() {
            let exceptions = vec!["*.local".to_string(), "10.0.0.0/8".to_string()];

            assert!(should_bypass_proxy(Some("127.0.0.1"), false, &[]));
            assert!(should_bypass_proxy(Some("::1"), false, &[]));
            assert!(should_bypass_proxy(
                Some("api.service.local"),
                false,
                &exceptions
            ));
            assert!(should_bypass_proxy(Some("10.24.1.8"), false, &exceptions));
            assert!(should_bypass_proxy(Some("printer"), true, &[]));
            assert!(!should_bypass_proxy(Some("chatgpt.com"), true, &exceptions));
        }

        #[test]
        fn simple_host_detection_excludes_local_service_names() {
            assert!(is_simple_hostname(Some("printer")));
            assert!(!is_simple_hostname(Some("chatgpt.com")));
            assert!(!is_simple_hostname(Some("127.0.0.1")));
        }

        #[tokio::test]
        async fn configured_proxy_keeps_loopback_direct() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let address = listener.local_addr().expect("loopback address");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept direct request");
                use std::io::{Read, Write};
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .expect("write response");
            });

            let response = apply_proxy_config(
                reqwest::Client::builder(),
                SystemProxyConfig {
                    http: Some("http://127.0.0.1:9".to_string()),
                    https: Some("http://127.0.0.1:9".to_string()),
                    ..SystemProxyConfig::default()
                },
            )
            .build()
            .expect("client")
            .get(format!("http://{address}"))
            .send()
            .await
            .expect("direct loopback request");

            assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
            server.join().expect("server");
        }
    }
}
