//! Shared reqwest helpers: URL-stripped errors, capped body reads, and a
//! rustls-only client factory plus 429/`Retry-After` parsing.

use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use thiserror::Error;

/// Upper bound for any parsed backoff (matching the Verda/RunPod 429 cap).
const MAX_RETRY_AFTER_SECS: f64 = 60.0;
/// Fallback when neither `Retry-After` nor `RateLimit` is present.
const DEFAULT_RETRY_AFTER_SECS: f64 = 5.0;

/// Build a rustls-only reqwest client. Never falls back to `Client::new()`.
///
/// `connect` sets `connect_timeout`; `request` sets the per-request `timeout`.
/// Both are optional — callers pass the durations their endpoint needs.
pub fn rustls_client(
    connect: Option<Duration>,
    request: Option<Duration>,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder().use_rustls_tls();
    if let Some(timeout) = connect {
        builder = builder.connect_timeout(timeout);
    }
    if let Some(timeout) = request {
        builder = builder.timeout(timeout);
    }
    builder.build()
}

/// Seconds to wait before retrying a rate-limited request.
///
/// Parses `Retry-After` first, then the IETF `RateLimit` header `reset=` field.
/// Values are clamped to `0..=60`; a missing or unparseable header defaults to
/// 5s. Never inspects URLs or bodies.
pub fn retry_after_seconds(headers: &HeaderMap) -> f64 {
    if let Some(v) = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
    {
        return v.clamp(0.0, MAX_RETRY_AFTER_SECS);
    }
    // IETF RateLimit header: "limit=…, remaining=…, reset=N"
    if let Some(raw) = headers.get("ratelimit").and_then(|v| v.to_str().ok()) {
        for part in raw.split(',') {
            let part = part.trim();
            if let Some(reset) = part
                .strip_prefix("reset=")
                .and_then(|s| s.trim().parse::<f64>().ok())
            {
                return reset.clamp(0.0, MAX_RETRY_AFTER_SECS);
            }
        }
    }
    DEFAULT_RETRY_AFTER_SECS
}

/// `reqwest::Error` `Display` includes the URL. Strip it before logs.
pub fn reqwest_error_for_log(err: reqwest::Error) -> String {
    err.without_url().to_string()
}

/// Bounded read of an upstream / probe response body.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProbeBodyError {
    /// `Content-Length` or accumulated chunks exceeded the configured cap.
    #[error("probe body too large")]
    TooLarge,
    /// Transport error before the body completed.
    #[error("probe body interrupted")]
    Interrupted,
    /// Bytes arrived but were not valid JSON for the probe.
    #[error("probe body parse")]
    Parse,
}

/// Read a reqwest response with a hard byte cap. Never logs the URL or body.
pub async fn read_reqwest_capped(
    mut resp: reqwest::Response,
    max_bytes: u64,
) -> Result<Bytes, ProbeBodyError> {
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(ProbeBodyError::TooLarge);
        }
    }
    let mut out = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let next = (out.len() as u64).saturating_add(chunk.len() as u64);
                if next > max_bytes {
                    return Err(ProbeBodyError::TooLarge);
                }
                out.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => return Err(ProbeBodyError::Interrupted),
        }
    }
    Ok(Bytes::from(out))
}

#[cfg(test)]
mod tests {
    use super::{
        read_reqwest_capped, reqwest_error_for_log, retry_after_seconds, rustls_client,
        ProbeBodyError,
    };
    use httpmock::prelude::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    #[test]
    fn retry_after_parses_retry_after_header() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(retry_after_seconds(&headers), 12.0);
    }

    #[test]
    fn retry_after_prefers_retry_after_over_ratelimit() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        headers.insert("ratelimit", HeaderValue::from_static("limit=10, reset=30"));
        assert_eq!(retry_after_seconds(&headers), 7.0);
    }

    #[test]
    fn retry_after_falls_back_to_ratelimit_reset() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "ratelimit",
            HeaderValue::from_static("limit=10, remaining=4, reset=25"),
        );
        assert_eq!(retry_after_seconds(&headers), 25.0);
    }

    #[test]
    fn retry_after_caps_at_sixty() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("900"));
        assert_eq!(retry_after_seconds(&headers), 60.0);
    }

    #[test]
    fn retry_after_floors_negative_at_zero() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("-3"));
        assert_eq!(retry_after_seconds(&headers), 0.0);
    }

    #[test]
    fn retry_after_defaults_to_five_when_missing_or_garbage() {
        assert_eq!(retry_after_seconds(&HeaderMap::new()), 5.0);
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("soon"));
        headers.insert("ratelimit", HeaderValue::from_static("limit=10"));
        assert_eq!(retry_after_seconds(&headers), 5.0);
    }

    #[test]
    fn rustls_client_builds_with_and_without_timeouts() {
        let bare = rustls_client(None, None).expect("bare client");
        drop(bare);
        let timed = rustls_client(
            Some(std::time::Duration::from_secs(5)),
            Some(std::time::Duration::from_secs(5)),
        )
        .expect("timed client");
        drop(timed);
    }

    #[tokio::test]
    async fn reqwest_error_for_log_strips_url() {
        let client = rustls_client(None, None).expect("client");
        let err = client
            .get("http://127.0.0.1:1/secret-share")
            .send()
            .await
            .expect_err("connect");
        let logged = reqwest_error_for_log(err);
        assert!(
            !logged.contains("secret-share"),
            "stripped error still has share token: {logged}"
        );
        assert!(
            !logged.contains("http://"),
            "stripped error still has url: {logged}"
        );
    }

    #[tokio::test]
    async fn read_reqwest_capped_rejects_content_length() {
        let server = MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(GET).path("/big");
            then.status(200)
                .header("content-length", "100")
                .body("x".repeat(100));
        });
        let client = rustls_client(None, None).expect("client");
        let resp = client
            .get(format!("{}/big", server.base_url()))
            .send()
            .await
            .expect("send");
        let err = read_reqwest_capped(resp, 8).await.expect_err("cap");
        assert_eq!(err, ProbeBodyError::TooLarge);
    }

    #[tokio::test]
    async fn read_reqwest_capped_accepts_small_body() {
        let server = MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body("hello");
        });
        let client = rustls_client(None, None).expect("client");
        let resp = client
            .get(format!("{}/ok", server.base_url()))
            .send()
            .await
            .expect("send");
        let bytes = read_reqwest_capped(resp, 32).await.expect("read");
        assert_eq!(bytes.as_ref(), b"hello");
    }
}
