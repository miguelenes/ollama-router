//! Bearer-key RunPod HTTP client. Never logs the API key or response bodies.

use std::time::Duration;

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use ollama_router_core::config::{OsEnv, RunpodConfig};
use ollama_router_core::http_util::{reqwest_error_for_log, retry_after_seconds, rustls_client};

use crate::types::{CatalogGpu, CatalogResponse, CreatePodRequest, Pod};

#[derive(Debug, thiserror::Error)]
pub enum RunpodError {
    #[error("{0}")]
    Message(String),
    #[error("runpod auth: {0}")]
    Auth(String),
    /// HTTP error from RunPod. Status + method + path only — never a response body.
    #[error("RunPod API error (status={status} on {method} {path})")]
    Http {
        status: u16,
        method: String,
        path: String,
    },
}

impl RunpodError {
    pub fn status(code: u16, method: &str, path: &str) -> Self {
        Self::Http {
            status: code,
            method: method.to_string(),
            path: path.to_string(),
        }
    }
}

pub struct RunpodClient {
    http: reqwest::Client,
    base_url_v1: String,
    base_url_v2: String,
    api_key: SecretString,
    cloud_type: String,
}

impl RunpodClient {
    pub fn new(config: RunpodConfig) -> Result<Self, RunpodError> {
        let key = config.api_key(&OsEnv).ok_or_else(|| {
            RunpodError::Auth(format!(
                "RunPod credentials missing: set {}",
                config.api_key_env
            ))
        })?;
        Self::with_api_key(config, key)
    }

    /// Test / injected credentials. Never log `api_key`.
    pub fn with_api_key(config: RunpodConfig, api_key: String) -> Result<Self, RunpodError> {
        let http = rustls_client(None, Some(Duration::from_secs(30)))
            .map_err(|err| RunpodError::Message(format!("http client: {err}")))?;
        Ok(Self {
            http,
            base_url_v1: config.base_url_v1.trim_end_matches('/').to_string(),
            base_url_v2: config.base_url_v2.trim_end_matches('/').to_string(),
            api_key: SecretString::from(api_key),
            cloud_type: config.cloud_type.trim().to_string(),
        })
    }

    async fn request_v1(
        &self,
        method: reqwest::Method,
        path: &str,
        json: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, RunpodError> {
        self.request(&self.base_url_v1, method, path, json, timeout)
            .await
    }

    async fn request_v2(
        &self,
        method: reqwest::Method,
        path: &str,
        json: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, RunpodError> {
        self.request(&self.base_url_v2, method, path, json, timeout)
            .await
    }

    async fn request(
        &self,
        base: &str,
        method: reqwest::Method,
        path: &str,
        json: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, RunpodError> {
        let url = format!("{base}{path}");
        let mut attempt = 0u32;
        loop {
            let mut builder = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(self.api_key.expose_secret());
            if let Some(body) = &json {
                builder = builder.json(body);
            }
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            let resp = match builder.send().await {
                Ok(resp) => resp,
                Err(err) => {
                    if attempt >= 3 {
                        return Err(RunpodError::Message(format!(
                            "RunPod request failed after retries: {}",
                            reqwest_error_for_log(err)
                        )));
                    }
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs_f64(
                        (2.0 * f64::from(attempt)).min(8.0),
                    ))
                    .await;
                    continue;
                }
            };
            let status = resp.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = retry_after_seconds(resp.headers());
                if attempt >= 3 {
                    return Err(RunpodError::status(429, method.as_str(), path));
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs_f64(retry_after)).await;
                continue;
            }
            if matches!(status.as_u16(), 408 | 425 | 500 | 502 | 503 | 504) && attempt < 1 {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if status.as_u16() >= 400 {
                return Err(RunpodError::status(status.as_u16(), method.as_str(), path));
            }
            return Ok(resp);
        }
    }

    pub async fn list_catalog_gpus(&self) -> Result<Vec<CatalogGpu>, RunpodError> {
        let cloud = self.cloud_type.trim();
        let mut path = String::from("/catalog/gpus?include=AVAILABILITY&product=POD");
        if !cloud.is_empty() {
            path.push_str("&cloud=");
            path.push_str(cloud);
        }
        let resp = self
            .request_v2(reqwest::Method::GET, &path, None, None)
            .await?;
        let parsed: CatalogResponse = resp
            .json()
            .await
            .map_err(|err| RunpodError::Message(format!("json: {err}")))?;
        Ok(parsed.gpus)
    }

    pub async fn list_pods(&self) -> Result<Vec<Pod>, RunpodError> {
        let resp = self
            .request_v1(reqwest::Method::GET, "/pods", None, None)
            .await?;
        let data: Value = resp
            .json()
            .await
            .map_err(|err| RunpodError::Message(format!("json: {err}")))?;
        as_pods(data)
    }

    pub async fn get_pod(&self, pod_id: &str) -> Result<Pod, RunpodError> {
        let path = format!("/pods/{pod_id}");
        let resp = self
            .request_v1(reqwest::Method::GET, &path, None, None)
            .await?;
        resp.json()
            .await
            .map_err(|err| RunpodError::Message(format!("json: {err}")))
    }

    pub async fn create_pod(&self, request: &CreatePodRequest) -> Result<Pod, RunpodError> {
        let resp = self
            .request_v1(
                reqwest::Method::POST,
                "/pods",
                Some(request.to_json()),
                Some(Duration::from_secs(60)),
            )
            .await?;
        resp.json()
            .await
            .map_err(|err| RunpodError::Message(format!("json: {err}")))
    }

    pub async fn delete_pod(&self, pod_id: &str) -> Result<bool, RunpodError> {
        let path = format!("/pods/{pod_id}");
        match self
            .request_v1(
                reqwest::Method::DELETE,
                &path,
                None,
                Some(Duration::from_secs(60)),
            )
            .await
        {
            Ok(resp) => Ok(matches!(resp.status().as_u16(), 200 | 202 | 204)),
            Err(RunpodError::Http {
                status: 404 | 204, ..
            }) => Ok(false),
            Err(err) => Err(err),
        }
    }
}

fn as_pods(data: Value) -> Result<Vec<Pod>, RunpodError> {
    let items = match data {
        Value::Array(items) => items,
        Value::Object(map) => {
            for key in ["pods", "data", "results", "items"] {
                if let Some(Value::Array(items)) = map.get(key) {
                    return items
                        .iter()
                        .cloned()
                        .map(|v| {
                            serde_json::from_value(v)
                                .map_err(|e| RunpodError::Message(e.to_string()))
                        })
                        .collect();
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(|v| serde_json::from_value(v).map_err(|e| RunpodError::Message(e.to_string())))
        .collect()
}

#[cfg(test)]
mod error_tests {
    use super::RunpodError;

    #[test]
    fn status_is_http_variant_without_body() {
        let err = RunpodError::status(503, "POST", "/pods");
        match &err {
            RunpodError::Http {
                status,
                method,
                path,
            } => {
                assert_eq!(*status, 503);
                assert_eq!(method, "POST");
                assert_eq!(path, "/pods");
            }
            other => panic!("expected Http, got {other}"),
        }
        assert_eq!(
            err.to_string(),
            "RunPod API error (status=503 on POST /pods)"
        );
    }
}
