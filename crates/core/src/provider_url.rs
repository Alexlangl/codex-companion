const PROVIDER_ENDPOINT_SUFFIXES: [&str; 6] = [
    "/v1/responses/compact",
    "/responses/compact",
    "/v1/chat/completions",
    "/chat/completions",
    "/v1/responses",
    "/responses",
];

pub fn provider_api_base_url(base_url: &str) -> String {
    let trimmed = base_url
        .trim()
        .split('?')
        .next()
        .unwrap_or(base_url)
        .trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    for suffix in PROVIDER_ENDPOINT_SUFFIXES {
        if let Some(root) = trimmed.strip_suffix(suffix) {
            return if suffix.starts_with("/v1/") {
                format!("{}/v1", root.trim_end_matches('/'))
            } else {
                root.trim_end_matches('/').to_string()
            };
        }
    }
    trimmed.to_string()
}

pub fn provider_base_url_is_endpoint(base_url: &str) -> bool {
    let path = base_url
        .split('?')
        .next()
        .unwrap_or(base_url)
        .trim()
        .trim_end_matches('/');
    PROVIDER_ENDPOINT_SUFFIXES
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

pub fn provider_endpoint_is_chat_completions(base_url: &str) -> bool {
    let path = base_url
        .split('?')
        .next()
        .unwrap_or(base_url)
        .trim()
        .trim_end_matches('/');
    path.ends_with("/chat/completions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_api_base_from_known_endpoints() {
        assert_eq!(
            provider_api_base_url("https://api.example.com/v1/chat/completions"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            provider_api_base_url("https://api.example.com/v1/responses"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            provider_api_base_url("https://api.example.com/responses"),
            "https://api.example.com"
        );
        assert_eq!(
            provider_api_base_url("https://api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            provider_api_base_url("https://api.example.com/v1/responses?api-version=2026-06-09"),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn detects_complete_endpoints() {
        assert!(provider_base_url_is_endpoint(
            "https://api.example.com/v1/chat/completions"
        ));
        assert!(provider_base_url_is_endpoint(
            "https://api.example.com/v1/responses/compact"
        ));
        assert!(provider_base_url_is_endpoint(
            "https://api.example.com/v1/responses?api-version=2026-06-09"
        ));
        assert!(!provider_base_url_is_endpoint("https://api.example.com/v1"));
        assert!(provider_endpoint_is_chat_completions(
            "https://api.example.com/v1/chat/completions"
        ));
        assert!(!provider_endpoint_is_chat_completions(
            "https://api.example.com/v1/responses"
        ));
    }
}
