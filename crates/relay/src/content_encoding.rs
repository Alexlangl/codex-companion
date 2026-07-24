use axum::http::{header, HeaderMap};
use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use std::fmt;
use std::io::{Cursor, Read};

pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 200 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestBodyDecodeError {
    TooLarge { limit: usize },
    UnsupportedEncoding(String),
    InvalidEncoding { encoding: String, message: String },
}

impl fmt::Display for RequestBodyDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(
                formatter,
                "decompressed request body exceeds the local {} MiB limit",
                limit / (1024 * 1024)
            ),
            Self::UnsupportedEncoding(encoding) => {
                write!(
                    formatter,
                    "unsupported request content-encoding: {encoding}"
                )
            }
            Self::InvalidEncoding { encoding, message } => {
                write!(
                    formatter,
                    "failed to decode request body as {encoding}: {message}"
                )
            }
        }
    }
}

pub(crate) fn decode_request_body(
    headers: &mut HeaderMap,
    body: Bytes,
    decoded_limit: usize,
) -> Result<Bytes, RequestBodyDecodeError> {
    let Some(encoding) = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(body);
    };

    let encodings = encoding
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let mut decoded = body.to_vec();
    for encoding in encodings.into_iter().rev() {
        decoded = decode_one(encoding, &decoded, decoded_limit)?;
    }

    headers.remove(header::CONTENT_ENCODING);
    headers.remove(header::CONTENT_LENGTH);
    headers.remove(header::TRANSFER_ENCODING);
    Ok(Bytes::from(decoded))
}

fn decode_one(
    encoding: &str,
    body: &[u8],
    decoded_limit: usize,
) -> Result<Vec<u8>, RequestBodyDecodeError> {
    match encoding.to_ascii_lowercase().as_str() {
        "identity" => bounded_bytes(body, decoded_limit),
        "gzip" | "x-gzip" => {
            read_bounded(GzDecoder::new(Cursor::new(body)), encoding, decoded_limit)
        }
        "deflate" => decode_deflate(body, decoded_limit),
        "br" => read_bounded(
            brotli::Decompressor::new(Cursor::new(body), 4096),
            encoding,
            decoded_limit,
        ),
        "zstd" | "zst" => {
            let decoder = zstd::stream::read::Decoder::new(Cursor::new(body)).map_err(|error| {
                RequestBodyDecodeError::InvalidEncoding {
                    encoding: encoding.to_string(),
                    message: error.to_string(),
                }
            })?;
            read_bounded(decoder, encoding, decoded_limit)
        }
        _ => Err(RequestBodyDecodeError::UnsupportedEncoding(
            encoding.to_string(),
        )),
    }
}

fn decode_deflate(body: &[u8], decoded_limit: usize) -> Result<Vec<u8>, RequestBodyDecodeError> {
    match read_bounded(
        ZlibDecoder::new(Cursor::new(body)),
        "deflate",
        decoded_limit,
    ) {
        Ok(decoded) => Ok(decoded),
        Err(RequestBodyDecodeError::InvalidEncoding { .. }) => read_bounded(
            DeflateDecoder::new(Cursor::new(body)),
            "deflate",
            decoded_limit,
        ),
        Err(error) => Err(error),
    }
}

fn bounded_bytes(body: &[u8], decoded_limit: usize) -> Result<Vec<u8>, RequestBodyDecodeError> {
    if body.len() > decoded_limit {
        return Err(RequestBodyDecodeError::TooLarge {
            limit: decoded_limit,
        });
    }
    Ok(body.to_vec())
}

fn read_bounded(
    reader: impl Read,
    encoding: &str,
    decoded_limit: usize,
) -> Result<Vec<u8>, RequestBodyDecodeError> {
    let mut output = Vec::new();
    reader
        .take(decoded_limit.saturating_add(1) as u64)
        .read_to_end(&mut output)
        .map_err(|error| RequestBodyDecodeError::InvalidEncoding {
            encoding: encoding.to_string(),
            message: error.to_string(),
        })?;
    if output.len() > decoded_limit {
        return Err(RequestBodyDecodeError::TooLarge {
            limit: decoded_limit,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    #[test]
    fn decodes_stacked_gzip_then_zstd_and_clears_entity_headers() {
        let payload = br#"{"model":"gpt-test","input":"hello"}"#;
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(payload).expect("gzip write");
        let gzip = gzip.finish().expect("gzip finish");
        let encoded = zstd::stream::encode_all(Cursor::new(gzip), 0).expect("zstd encode");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_ENCODING,
            "gzip, zstd".parse().expect("encoding"),
        );
        headers.insert(header::CONTENT_LENGTH, "123".parse().expect("length"));

        let decoded =
            decode_request_body(&mut headers, Bytes::from(encoded), 1024).expect("decode request");

        assert_eq!(&decoded[..], payload);
        assert!(!headers.contains_key(header::CONTENT_ENCODING));
        assert!(!headers.contains_key(header::CONTENT_LENGTH));
    }

    #[test]
    fn rejects_decompressed_bodies_over_the_limit() {
        let encoded =
            zstd::stream::encode_all(Cursor::new(vec![b'x'; 2048]), 0).expect("zstd encode");
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_ENCODING, "zstd".parse().expect("encoding"));

        let error = decode_request_body(&mut headers, Bytes::from(encoded), 1024)
            .expect_err("body should exceed decoded limit");

        assert_eq!(error, RequestBodyDecodeError::TooLarge { limit: 1024 });
    }

    #[test]
    fn rejects_unknown_content_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_ENCODING,
            "snappy".parse().expect("encoding"),
        );

        let error = decode_request_body(&mut headers, Bytes::from_static(b"payload"), 1024)
            .expect_err("encoding should be rejected");

        assert_eq!(
            error,
            RequestBodyDecodeError::UnsupportedEncoding("snappy".to_string())
        );
    }
}
