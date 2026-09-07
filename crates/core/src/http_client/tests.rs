use super::*;
use std::net::TcpListener;

#[test]
fn explicit_proxy_wins_and_both_environment_spellings_agree() {
    let env = std::collections::BTreeMap::from([
        ("https_proxy", "http://explicit.example:8080"),
        ("HTTPS_PROXY", "http://ignored.example:8081"),
        ("no_proxy", "internal.example"),
    ]);
    let config = resolve_proxy_config(
        |key| env.get(key).map(|v| (*v).to_string()),
        || panic!("explicit proxies must skip system lookup"),
    );
    assert_eq!(
        config
            .proxy_for(&url::Url::parse("https://public.example").unwrap())
            .as_deref(),
        Some("http://explicit.example:8080")
    );
    assert!(config
        .proxy_for(&url::Url::parse("https://api.internal.example").unwrap())
        .is_none());
    assert!(config
        .proxy_for(&url::Url::parse("http://public.example").unwrap())
        .is_none());
    let child = config
        .environment()
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(child["HTTPS_PROXY"], child["https_proxy"]);
    assert_eq!(child["NO_PROXY"], child["no_proxy"]);
    assert!(child["NO_PROXY"].contains("127.0.0.1"));
    assert!(child["NO_PROXY"].contains("internal.example"));
}

#[test]
fn all_proxy_and_system_settings_share_loopback_protection() {
    let explicit = resolve_proxy_config(
        |key| (key == "ALL_PROXY").then(|| "socks5h://localhost:1080".to_string()),
        || panic!("no system lookup"),
    );
    for url in ["http://public.example", "https://public.example"] {
        assert_eq!(
            explicit
                .proxy_for(&url::Url::parse(url).unwrap())
                .as_deref(),
            Some("socks5h://localhost:1080")
        );
    }
    for url in [
        "http://127.0.0.2:17687",
        "http://[::1]:17687",
        "http://localhost:17687",
    ] {
        assert!(explicit.proxy_for(&url::Url::parse(url).unwrap()).is_none());
    }
    let system = resolve_proxy_config(
        |_| None,
        || ProxyConfig {
            https: Some("http://system.example:8080".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(
        system
            .environment()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()["HTTPS_PROXY"],
        "http://system.example:8080"
    );
    assert!(ProxyConfig::default()
        .proxy_for(&url::Url::parse("https://public.example").unwrap())
        .is_none());
}

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
        ProxyConfig {
            http: Some("http://127.0.0.1:9".to_string()),
            https: Some("http://127.0.0.1:9".to_string()),
            ..ProxyConfig::default()
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

#[tokio::test]
async fn socks_proxy_resolves_remote_host_and_transports_http() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 5);
        let count = stream.read_u8().await.unwrap();
        let mut methods = vec![0; count as usize];
        stream.read_exact(&mut methods).await.unwrap();
        assert!(methods.contains(&0));
        stream.write_all(&[5, 0]).await.unwrap();
        let mut header = [0; 4];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(header, [5, 1, 0, 3]); // Remote DNS, not local resolution.
        let length = stream.read_u8().await.unwrap();
        let mut host = vec![0; length as usize];
        stream.read_exact(&mut host).await.unwrap();
        assert_eq!(host, b"unreachable.example.test");
        assert_eq!(stream.read_u16().await.unwrap(), 80);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 16384);
        }
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });
    let client = apply_proxy_config(
        reqwest::Client::builder(),
        ProxyConfig {
            all: Some(format!("socks5h://{address}")),
            ..Default::default()
        },
    )
    .timeout(std::time::Duration::from_secs(5))
    .build()
    .unwrap();
    let response = client
        .get("http://unreachable.example.test")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    server.await.unwrap();
}
