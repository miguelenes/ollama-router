//! OAuth2 client-credentials Verda HTTP client. Never logs tokens or bodies.

use std::time::{Duration, Instant};

use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::Mutex;

use ollama_router_core::config::{OsEnv, VerdaConfig};

use crate::types::{Image, Instance, InstanceAvailability, InstanceType, SshKey, TokenResponse};

const TOKEN_PATH: &str = "/v1/oauth2/token";
const TOKEN_LEEWAY: Duration = Duration::from_secs(30);
const MAX_429_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum VerdaError {
    #[error("{0}")]
    Message(String),
    #[error("verda auth: {0}")]
    Auth(String),
}

impl VerdaError {
    pub fn status(code: u16, method: &str, path: &str) -> Self {
        Self::Message(format!(
            "Verda API error (status={code} on {method} {path})"
        ))
    }
}

pub struct VerdaClient {
    http: reqwest::Client,
    base_url: String,
    client_id: String,
    client_secret: String,
    access_token: Mutex<Option<String>>,
    refresh_token: Mutex<Option<String>>,
    expires_at: Mutex<Instant>,
    auth_lock: Mutex<()>,
}

impl VerdaClient {
    pub fn new(config: VerdaConfig) -> Result<Self, VerdaError> {
        let env = OsEnv;
        let id = config.client_id(&env).ok_or_else(|| {
            VerdaError::Auth(format!(
                "Verda credentials missing: set {}",
                config.client_id_env
            ))
        })?;
        let secret = config.client_secret(&env).ok_or_else(|| {
            VerdaError::Auth(format!(
                "Verda credentials missing: set {}",
                config.client_secret_env
            ))
        })?;
        Self::with_credentials(config, id, secret)
    }

    /// Test / injected credentials. Never log `client_secret`.
    pub fn with_credentials(
        config: VerdaConfig,
        client_id: String,
        client_secret: String,
    ) -> Result<Self, VerdaError> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| VerdaError::Message(format!("http client: {err}")))?;
        Ok(Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            http,
            client_id,
            client_secret,
            access_token: Mutex::new(None),
            refresh_token: Mutex::new(None),
            expires_at: Mutex::new(Instant::now()),
            auth_lock: Mutex::new(()),
        })
    }

    fn credentials(&self) -> (String, String) {
        (self.client_id.clone(), self.client_secret.clone())
    }

    async fn token_request(&self, payload: Value) -> Result<TokenResponse, VerdaError> {
        let resp = self
            .http
            .post(format!("{}{TOKEN_PATH}", self.base_url))
            .json(&payload)
            .send()
            .await
            .map_err(|err| VerdaError::Message(format!("Verda token request failed: {err}")))?;
        if resp.status().is_client_error() || resp.status().is_server_error() {
            return Err(VerdaError::Auth(format!(
                "Verda token request rejected (status={})",
                resp.status().as_u16()
            )));
        }
        let token: TokenResponse = resp
            .json()
            .await
            .map_err(|_| VerdaError::Message("Verda token response malformed".into()))?;
        if token.access_token.is_empty() {
            return Err(VerdaError::Auth(
                "Verda token response missing access_token".into(),
            ));
        }
        Ok(token)
    }

    async fn store_token(&self, token: TokenResponse) {
        *self.access_token.lock().await = Some(token.access_token);
        if let Some(refresh) = token.refresh_token {
            *self.refresh_token.lock().await = Some(refresh);
        }
        let expires_in = token.expires_in.unwrap_or(3600).max(30);
        *self.expires_at.lock().await = Instant::now() + Duration::from_secs(expires_in);
        tracing::info!(expires_in_seconds = expires_in, "verda_token_refreshed");
    }

    async fn authenticate(&self) -> Result<(), VerdaError> {
        let (client_id, client_secret) = self.credentials();
        let token = self
            .token_request(serde_json::json!({
                "grant_type": "client_credentials",
                "client_id": client_id,
                "client_secret": client_secret,
            }))
            .await?;
        self.store_token(token).await;
        Ok(())
    }

    async fn refresh(&self) -> Result<(), VerdaError> {
        let refresh = self.refresh_token.lock().await.clone().ok_or_else(|| {
            VerdaError::Auth("Verda refresh failed: no refresh_token held".into())
        })?;
        let token = self
            .token_request(serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh,
            }))
            .await?;
        self.store_token(token).await;
        Ok(())
    }

    async fn ensure_token(&self) -> Result<(), VerdaError> {
        let _guard = self.auth_lock.lock().await;
        let expiring = self.access_token.lock().await.is_none()
            || Instant::now() + TOKEN_LEEWAY >= *self.expires_at.lock().await;
        if !expiring {
            return Ok(());
        }
        match self.refresh().await {
            Ok(()) => Ok(()),
            Err(VerdaError::Auth(_)) => self.authenticate().await,
            Err(err) => Err(err),
        }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        json: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, VerdaError> {
        self.ensure_token().await?;
        let url = format!("{}{path}", self.base_url);
        let mut attempt = 0u32;
        let mut refreshed_once = false;
        loop {
            let token = self.access_token.lock().await.clone().unwrap_or_default();
            let mut builder = self.http.request(method.clone(), &url).bearer_auth(&token);
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
                        return Err(VerdaError::Message(format!(
                            "Verda request failed after retries: {err}"
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
            if status == StatusCode::UNAUTHORIZED && !refreshed_once {
                refreshed_once = true;
                *self.access_token.lock().await = None;
                self.ensure_token().await.map_err(|_| {
                    VerdaError::Auth("Verda authorization failed after refresh".into())
                })?;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = resp
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(5.0);
                if attempt >= 3 {
                    return Err(VerdaError::Message(format!(
                        "Verda rate limited (status=429, retry_after={retry_after})"
                    )));
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs_f64(
                    retry_after.min(MAX_429_WAIT.as_secs_f64()),
                ))
                .await;
                continue;
            }
            if matches!(status.as_u16(), 408 | 425 | 500 | 502 | 503 | 504) && attempt < 1 {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            if status.as_u16() >= 400 {
                return Err(VerdaError::status(status.as_u16(), method.as_str(), path));
            }
            return Ok(resp);
        }
    }

    async fn get_list(&self, path: &str) -> Result<Vec<Value>, VerdaError> {
        let resp = self.request(reqwest::Method::GET, path, None, None).await?;
        let data: Value = resp
            .json()
            .await
            .map_err(|err| VerdaError::Message(format!("json: {err}")))?;
        Ok(as_list(data))
    }

    pub async fn get_instance_availability(&self) -> Result<Vec<InstanceAvailability>, VerdaError> {
        self.get_list("/v1/instance-availability")
            .await?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| VerdaError::Message(e.to_string())))
            .collect()
    }

    pub async fn get_instance_types(&self) -> Result<Vec<InstanceType>, VerdaError> {
        self.get_list("/v1/instance-types")
            .await?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| VerdaError::Message(e.to_string())))
            .collect()
    }

    pub async fn confirm_availability(&self, instance_type: &str) -> Option<bool> {
        let path = format!("/v1/instance-availability/{instance_type}");
        let resp = self
            .request(reqwest::Method::GET, &path, None, None)
            .await
            .ok()?;
        resp.json::<Value>().await.ok()?.as_bool()
    }

    pub async fn get_images(&self) -> Result<Vec<Image>, VerdaError> {
        self.get_list("/v1/images")
            .await?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| VerdaError::Message(e.to_string())))
            .collect()
    }

    pub async fn list_ssh_keys(&self) -> Result<Vec<SshKey>, VerdaError> {
        for path in ["/v1/sshkeys", "/v1/ssh-keys"] {
            if let Ok(items) = self.get_list(path).await {
                return items
                    .into_iter()
                    .map(|v| {
                        serde_json::from_value(v).map_err(|e| VerdaError::Message(e.to_string()))
                    })
                    .collect();
            }
        }
        Err(VerdaError::Message(
            "Verda ssh keys endpoint unavailable (both /v1/sshkeys and /v1/ssh-keys failed)".into(),
        ))
    }

    pub async fn create_ssh_key(&self, name: &str, key: &str) -> Result<SshKey, VerdaError> {
        let resp = self
            .request(
                reqwest::Method::POST,
                "/v1/sshkeys",
                Some(serde_json::json!({"name": name, "key": key})),
                Some(Duration::from_secs(60)),
            )
            .await?;
        resp.json()
            .await
            .map_err(|err| VerdaError::Message(format!("json: {err}")))
    }

    pub async fn list_instances(&self) -> Result<Vec<Instance>, VerdaError> {
        self.get_list("/v1/instances")
            .await?
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| VerdaError::Message(e.to_string())))
            .collect()
    }

    pub async fn get_instance(&self, instance_id: &str) -> Result<Instance, VerdaError> {
        let resp = self
            .request(
                reqwest::Method::GET,
                &format!("/v1/instances/{instance_id}"),
                None,
                None,
            )
            .await?;
        resp.json()
            .await
            .map_err(|err| VerdaError::Message(format!("json: {err}")))
    }

    pub async fn create_instance(&self, payload: Value) -> Result<Instance, VerdaError> {
        let resp = self
            .request(
                reqwest::Method::POST,
                "/v1/instances",
                Some(payload),
                Some(Duration::from_secs(60)),
            )
            .await?;
        let text = resp
            .text()
            .await
            .map_err(|err| VerdaError::Message(format!("body: {err}")))?;
        let trimmed = text.trim();
        if trimmed.starts_with('{') {
            serde_json::from_str(trimmed).map_err(|e| VerdaError::Message(e.to_string()))
        } else {
            let id = trimmed.trim_matches('"').to_string();
            Ok(Instance {
                id: Some(id),
                ..Instance::default()
            })
        }
    }

    pub async fn delete_instance(
        &self,
        instance_id: &str,
        volume_ids: Option<Vec<String>>,
        delete_permanently: bool,
    ) -> Result<bool, VerdaError> {
        let mut payload = serde_json::json!({
            "action": "delete",
            "id": instance_id,
            "delete_permanently": delete_permanently,
        });
        if let Some(ids) = volume_ids {
            payload["volume_ids"] = Value::Array(ids.into_iter().map(Value::String).collect());
        }
        match self
            .request(
                reqwest::Method::PUT,
                "/v1/instances",
                Some(payload),
                Some(Duration::from_secs(60)),
            )
            .await
        {
            Ok(resp) => Ok(matches!(resp.status().as_u16(), 200 | 202 | 204)),
            Err(VerdaError::Message(msg))
                if msg.contains("status=404") || msg.contains("status=204") =>
            {
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }
}

fn as_list(data: Value) -> Vec<Value> {
    match data {
        Value::Array(items) => items,
        Value::Object(map) => {
            for key in [
                "data",
                "results",
                "items",
                "instances",
                "ssh_keys",
                "images",
                "scripts",
            ] {
                if let Some(Value::Array(items)) = map.get(key) {
                    return items.clone();
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}
