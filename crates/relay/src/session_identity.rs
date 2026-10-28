use axum::http::HeaderMap;
use codex_companion_core::ProviderConfig;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn mapped(provider: &ProviderConfig, value: &str) -> String {
    let account = codex_companion_provider::account_identity_key(provider);
    let digest = Sha256::digest(format!(
        "codex-companion-session-v1:{}:{account}:{value}",
        account.len()
    ));
    // A stable UUID-shaped identifier isolates the same local thread per account.
    let mut b: [u8; 16] = digest[..16].try_into().unwrap();
    b[6] = (b[6] & 15) | 80;
    b[8] = (b[8] & 63) | 128;
    let h = b.iter().map(|v| format!("{v:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

pub(crate) fn headers(provider: &ProviderConfig, headers: &mut HeaderMap, enabled: bool) {
    if !enabled {
        return;
    }
    for key in [
        "session-id",
        "session_id",
        "thread-id",
        "thread_id",
        "x-session-id",
        "x-amp-thread-id",
    ] {
        if let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) {
            if let Ok(value) = mapped(provider, value).parse() {
                headers.insert(key, value);
            }
        }
    }
}

pub(crate) fn body(provider: &ProviderConfig, value: &mut Value, enabled: bool) {
    if !enabled {
        return;
    }
    fn request(provider: &ProviderConfig, value: &mut Value) {
        for key in ["prompt_cache_key", "session_id", "thread_id"] {
            if let Some(original) = value.get(key).and_then(Value::as_str) {
                value[key] = Value::String(mapped(provider, original));
            }
        }
    }
    request(provider, value);
    for key in ["response", "session"] {
        if let Some(value) = value.get_mut(key) {
            request(provider, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::ProviderKind;
    use serde_json::json;
    fn provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            name: id.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.invalid"),
            auth_ref: Some(format!("env:{id}")),
            websocket_url: None,
            direct_auth_ref: None,
            model_map: Default::default(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: 300,
            account: None,
        }
    }
    #[test]
    fn stable_per_account_and_opt_out_preserves_wire_values() {
        let a = provider("a");
        let b = provider("b");
        assert_eq!(mapped(&a, "thread"), mapped(&a, "thread"));
        assert_ne!(mapped(&a, "thread"), mapped(&b, "thread"));
        let original =
            json!({"response":{"prompt_cache_key":"thread"},"input":[{"thread_id":"opaque"}]});
        let mut value = original.clone();
        body(&a, &mut value, false);
        assert_eq!(value, original);
        body(&a, &mut value, true);
        assert_eq!(value["input"], original["input"]);
        let mut headers = HeaderMap::new();
        headers.insert("session-id", "thread".parse().unwrap());
        headers.insert("thread-id", "thread".parse().unwrap());
        self::headers(&a, &mut headers, false);
        assert_eq!(headers["session-id"], "thread");
        self::headers(&a, &mut headers, true);
        assert_eq!(headers["session-id"], headers["thread-id"]);
        assert_eq!(
            value["response"]["prompt_cache_key"].as_str(),
            headers["session-id"].to_str().ok()
        );
    }
}
