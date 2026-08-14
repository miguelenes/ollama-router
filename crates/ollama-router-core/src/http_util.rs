//! Shared reqwest helpers: URL-stripped errors and capped body reads.

use bytes::Bytes;
use thiserror::Error;

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
    use super::{read_reqwest_capped, reqwest_error_for_log, ProbeBodyError};
    use httpmock::prelude::*;

    #[tokio::test]
    async fn reqwest_error_for_log_strips_url() {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("client");
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
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("client");
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
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("client");
        let resp = client
            .get(format!("{}/ok", server.base_url()))
            .send()
            .await
            .expect("send");
        let bytes = read_reqwest_capped(resp, 32).await.expect("read");
        assert_eq!(bytes.as_ref(), b"hello");
    }
}
