use std::fmt;

/// Reads an HTTP response body without allowing an untrusted provider to make
/// the daemon allocate an unbounded amount of memory.
pub(crate) async fn read_response_bytes_limited(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, ResponseBodyReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(ResponseBodyReadError::TooLarge { max_bytes });
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(max_bytes as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(ResponseBodyReadError::Read)?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ResponseBodyReadError::TooLarge { max_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug)]
pub(crate) enum ResponseBodyReadError {
    TooLarge { max_bytes: usize },
    Read(reqwest::Error),
}

impl fmt::Display for ResponseBodyReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { max_bytes } => {
                write!(
                    formatter,
                    "响应超过 {} 限制",
                    display_byte_limit(*max_bytes)
                )
            }
            Self::Read(source) => write!(formatter, "读取响应失败: {source}"),
        }
    }
}

fn display_byte_limit(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let (unit, label) = if bytes < MIB {
        (KIB, "KiB")
    } else {
        (MIB, "MiB")
    };
    let whole = bytes / unit;
    let remainder = bytes % unit;
    if remainder == 0 {
        return format!("{whole} {label}");
    }
    let tenths = (remainder.saturating_mul(10) + unit / 2) / unit;
    if tenths == 10 {
        return format!("{} {label}", whole + 1);
    }
    format!("{whole}.{tenths} {label}")
}

impl std::error::Error for ResponseBodyReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge { .. } => None,
            Self::Read(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, response::Response, routing::get, Router};
    use bytes::Bytes;
    use futures_util::stream;
    use std::convert::Infallible;

    #[test]
    fn reports_limits_in_human_readable_units() {
        assert_eq!(
            ResponseBodyReadError::TooLarge {
                max_bytes: 64 * 1024
            }
            .to_string(),
            "响应超过 64 KiB 限制"
        );
        assert_eq!(
            ResponseBodyReadError::TooLarge { max_bytes: 512 }.to_string(),
            "响应超过 512 B 限制"
        );
        assert_eq!(
            ResponseBodyReadError::TooLarge { max_bytes: 1536 }.to_string(),
            "响应超过 1.5 KiB 限制"
        );
        assert_eq!(
            ResponseBodyReadError::TooLarge {
                max_bytes: 3 * 1024 * 1024
            }
            .to_string(),
            "响应超过 3 MiB 限制"
        );
    }

    #[tokio::test]
    async fn stops_an_oversized_chunked_response_without_collecting_it() {
        let app = Router::new().route(
            "/",
            get(|| async {
                Response::builder()
                    .body(Body::from_stream(stream::once(async {
                        Ok::<_, Infallible>(Bytes::from("x".repeat(1_025)))
                    })))
                    .expect("response")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("response");
        let error = read_response_bytes_limited(response, 1_024)
            .await
            .expect_err("chunked body should exceed the limit");

        assert!(matches!(
            error,
            ResponseBodyReadError::TooLarge { max_bytes: 1_024 }
        ));
        server.abort();
    }
}
