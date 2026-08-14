//! Soft-fail HTTP client for `/v1/capacity` and `/v1/pressure`.

use std::time::Duration;

use thiserror::Error;
use url::Url;

use ollama_capacity_types::{CapacityReport, PressureEnvelope};

use crate::http_util::{read_reqwest_capped, ProbeBodyError};

/// Allowlisted probe failure. Never includes bodies, tokens, or URLs.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CapacityError {
    #[error("capacity agent http {status}")]
    Http { status: u16 },
    #[error("capacity agent timeout")]
    Timeout,
    #[error("capacity agent unreachable")]
    Unreachable,
    #[error("capacity agent parse")]
    Parse,
}

impl CapacityError {
    /// Short token for `capacity_error` (no bodies).
    pub fn as_reason(&self) -> &'static str {
        match self {
            Self::Http { .. } => "http_status",
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Parse => "parse",
        }
    }

    fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout
        } else {
            Self::Unreachable
        }
    }
}

/// Derived agent URLs for one Ollama node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapacityTarget {
    pub capacity_url: String,
    pub pressure_url: String,
}

/// Successful (or partially successful) agent probe.
#[derive(Clone, Debug)]
pub struct CapacityProbe {
    pub report: CapacityReport,
    /// Agent `pressure_level` token when `/v1/pressure` succeeded.
    pub pressure_level: Option<String>,
}

/// Shared reqwest client (rustls). Parse with `bytes` + `serde_json`.
#[derive(Clone, Debug)]
pub struct CapacityClient {
    inner: reqwest::Client,
}

impl CapacityClient {
    /// Wrap an existing rustls client (typically the binary's upstream client).
    pub fn new(inner: reqwest::Client) -> Self {
        Self { inner }
    }

    /// GET capacity, then optionally `/v1/pressure` for `pressure_level`.
    ///
    /// A pressure miss does not fail the probe: nested `pressure` on the
    /// capacity document is still returned. Callers must not flip health.
    pub async fn probe(
        &self,
        target: &CapacityTarget,
        token: Option<&str>,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<CapacityProbe, CapacityError> {
        let report = self
            .get_json::<CapacityReport>(&target.capacity_url, token, timeout, max_bytes)
            .await?;
        let pressure_level = match self
            .get_json::<PressureEnvelope>(&target.pressure_url, token, timeout, max_bytes)
            .await
        {
            Ok(envelope) => envelope.pressure_level,
            Err(_) => None,
        };
        Ok(CapacityProbe {
            report,
            pressure_level,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: Option<&str>,
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<T, CapacityError> {
        let mut req = self.inner.get(url).timeout(timeout);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.map_err(CapacityError::from_reqwest)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CapacityError::Http {
                status: status.as_u16(),
            });
        }
        let bytes = match read_reqwest_capped(resp, max_bytes).await {
            Ok(bytes) => bytes,
            Err(ProbeBodyError::TooLarge | ProbeBodyError::Interrupted | ProbeBodyError::Parse) => {
                return Err(CapacityError::Parse);
            }
        };
        serde_json::from_slice(&bytes).map_err(|_| CapacityError::Parse)
    }
}

/// Build agent URLs from an explicit `capacity_url` or `{ollama-host}:{port}{path}`.
pub fn capacity_target(
    ollama_url: Option<&str>,
    explicit_capacity_url: Option<&str>,
    port: u16,
    capacity_path: &str,
    pressure_path: &str,
) -> Option<CapacityTarget> {
    let capacity_url = if let Some(explicit) = explicit_capacity_url {
        let trimmed = explicit.trim();
        if trimmed.is_empty() {
            derive_agent_url(ollama_url?, port, capacity_path)?
        } else {
            trimmed.to_string()
        }
    } else {
        derive_agent_url(ollama_url?, port, capacity_path)?
    };
    let pressure_url = replace_path(&capacity_url, pressure_path)
        .or_else(|| derive_agent_url(ollama_url?, port, pressure_path))?;
    Some(CapacityTarget {
        capacity_url,
        pressure_url,
    })
}

fn derive_agent_url(ollama_url: &str, port: u16, path: &str) -> Option<String> {
    let mut parsed = Url::parse(ollama_url.trim()).ok()?;
    parsed.set_port(Some(port)).ok()?;
    parsed.set_path(path);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn replace_path(url: &str, path: &str) -> Option<String> {
    let mut parsed = Url::parse(url).ok()?;
    parsed.set_path(path);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}
