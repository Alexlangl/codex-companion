use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::Response,
};
use bytes::Bytes;
use codex_companion_core::{ProviderConfig, ProviderKind};
use codex_companion_provider::{ensure_codex_auth_snapshot, resolve_auth_token};
use futures_util::TryStreamExt;

pub(crate) async fn send_upstream(
    client: &reqwest::Client,
    provider: &ProviderConfig,
    method: &Method,
    headers: &HeaderMap,
    body: Bytes,
    upstream: &str,
) -> std::result::Result<reqwest::Response, String> {
    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|error| format!("invalid method: {error}"))?;
    let mut request = client.request(reqwest_method, upstream);
    for (name, value) in headers {
        if matches!(
            *name,
            header::HOST | header::AUTHORIZATION | header::CONTENT_LENGTH
        ) {
            continue;
        }
        request = request.header(name, value);
    }
    if provider.kind == ProviderKind::OfficialCodex {
        let auth = ensure_codex_auth_snapshot(provider)
            .await
            .map_err(|error| error.to_string())?;
        request = request.bearer_auth(auth.access_token);
        if let Some(account_id) = provider
            .account
            .as_ref()
            .and_then(|account| account.account_id.clone())
            .or(auth.account_id)
        {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        request = request.header("originator", "codex-companion");
    } else if let Some(token) = resolve_auth_token(provider) {
        request = request.bearer_auth(token);
    }
    let body = rewrite_model(provider, body);
    request
        .body(body)
        .send()
        .await
        .map_err(|error| error.to_string())
}

fn rewrite_model(provider: &ProviderConfig, body: Bytes) -> Bytes {
    if provider.model_map.is_empty() {
        return body;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(model) = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
    else {
        return body;
    };
    let Some(mapped) = provider.model_map.get(&model).cloned().or_else(|| {
        provider
            .model_map
            .get("default")
            .filter(|mapped| !mapped.trim().is_empty())
            .cloned()
    }) else {
        return body;
    };
    if mapped == model {
        return body;
    }
    value["model"] = serde_json::Value::String(mapped);
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

pub(crate) fn stream_response(provider_id: String, upstream: reqwest::Response) -> Response {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream
        .bytes_stream()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error));
    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if matches!(*name, header::CONNECTION | header::TRANSFER_ENCODING) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .header("x-codex-companion-provider", provider_id)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|error| {
            text_response(
                StatusCode::BAD_GATEWAY,
                format!("response build failed: {error}"),
            )
        })
}

pub(crate) fn text_response(status: StatusCode, text: impl Into<String>) -> Response {
    Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(text.into()))
        .expect("response")
}

pub(crate) fn upstream_url(provider: &ProviderConfig, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let upstream_path = if path == "/v1" {
        "/".to_string()
    } else {
        path.strip_prefix("/v1/")
            .map(|value| {
                let mut output = String::from("/");
                output.push_str(value);
                output
            })
            .unwrap_or_else(|| path.to_string())
    };
    let mut url = format!(
        "{}{}",
        provider.base_url.trim_end_matches('/'),
        upstream_path
    );
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    if provider.kind == ProviderKind::OfficialCodex {
        url = append_client_version(url);
    }
    url
}

fn append_client_version(mut url: String) -> String {
    if url.contains("client_version=") {
        return url;
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    url.push(separator);
    url.push_str("client_version=");
    url.push_str(env!("CARGO_PKG_VERSION"));
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_companion_core::{default_refresh_interval_seconds, ProviderKind};
    use std::collections::BTreeMap;

    #[test]
    fn builds_upstream_url_from_v1_base() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/chat/completions?stream=true".parse().expect("uri");
        assert_eq!(
            upstream_url(&provider, &uri),
            "https://api.example.com/v1/chat/completions?stream=true"
        );
    }

    #[test]
    fn rewrites_request_model_from_provider_map() {
        let mut model_map = BTreeMap::new();
        model_map.insert("gpt-5-codex".to_string(), "deepseek-chat".to_string());
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://api.example.com/v1".to_string(),
            auth_ref: None,
            direct_auth_ref: None,
            model_map,
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let body = rewrite_model(
            &provider,
            Bytes::from_static(br#"{"model":"gpt-5-codex","input":"hello"}"#),
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["model"], "deepseek-chat");
    }

    #[test]
    fn official_codex_url_gets_client_version() {
        let provider = ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind: ProviderKind::OfficialCodex,
            base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: default_refresh_interval_seconds(),
            account: None,
        };
        let uri: Uri = "/v1/models?foo=bar".parse().expect("uri");
        assert_eq!(
            upstream_url(&provider, &uri),
            format!(
                "https://chatgpt.com/backend-api/codex/models?foo=bar&client_version={}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
