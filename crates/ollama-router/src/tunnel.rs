//! Router-side zrok private access frontends bound to loopback.
//!
//! Tests use [`TunnelFrontends::loopback`] (no `zrok` binary). Production uses
//! [`TunnelFrontends::from_config`]. Never log share or enable tokens.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use ollama_router_core::config::{http_url_for_bind, socket_addr_for_bind, TunnelConfig};
use ollama_router_core::fleet::{FleetState, NodeId, Registry};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// v1 CLI: bind a private access frontend on loopback.
pub fn access_private_args(token: &str, bind: &str) -> Vec<String> {
    vec![
        "access".into(),
        "private".into(),
        "--headless".into(),
        "--bindAddress".into(),
        bind.to_string(),
        token.to_string(),
    ]
}

/// Env pairs for a `zrok` child (never includes tokens).
pub fn zrok_process_env(api_endpoint: Option<&str>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(endpoint) = api_endpoint.map(str::trim).filter(|s| !s.is_empty()) {
        env.push(("ZROK_API_ENDPOINT".to_string(), endpoint.to_string()));
    }
    env
}

fn apply_zrok_env(cmd: &mut Command, api_endpoint: Option<&str>) {
    for (key, value) in zrok_process_env(api_endpoint) {
        cmd.env(key, value);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrontendKind {
    Loopback,
    Zrok,
}

struct BoundFrontend {
    port: u16,
    _hold: Option<TcpListener>,
    child: Option<Child>,
    serve: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for BoundFrontend {
    fn drop(&mut self) {
        if let Some(handle) = self.serve.take() {
            handle.abort();
        }
    }
}

struct Inner {
    by_share: HashMap<String, BoundFrontend>,
    enabled: bool,
}

/// Shared map of share-id → loopback port (reuse on re-enroll).
#[derive(Clone)]
pub struct TunnelFrontends {
    kind: FrontendKind,
    zrok_bin: String,
    api_endpoint: Option<String>,
    enable_token_env: String,
    access_bind: String,
    inner: Arc<Mutex<Inner>>,
}

impl TunnelFrontends {
    /// Bind ephemeral loopback listeners; do not spawn `zrok`.
    pub fn loopback() -> Self {
        Self {
            kind: FrontendKind::Loopback,
            zrok_bin: String::new(),
            api_endpoint: None,
            enable_token_env: String::new(),
            access_bind: "127.0.0.1".into(),
            inner: Arc::new(Mutex::new(Inner {
                by_share: HashMap::new(),
                enabled: true,
            })),
        }
    }

    /// Spawn `zrok access private` using tunables (`access_bind`, API endpoint).
    pub fn from_config(cfg: &TunnelConfig) -> Self {
        Self {
            kind: FrontendKind::Zrok,
            zrok_bin: cfg.zrok_bin.clone(),
            api_endpoint: cfg.api_endpoint().map(str::to_string),
            enable_token_env: cfg.enable_token_env.clone(),
            access_bind: cfg.access_bind.trim().to_string(),
            inner: Arc::new(Mutex::new(Inner {
                by_share: HashMap::new(),
                enabled: false,
            })),
        }
    }

    /// Spawn `zrok access private` bound to `127.0.0.1:<port>`.
    pub fn zrok(bin: impl Into<String>) -> Self {
        Self::from_config(&TunnelConfig {
            zrok_bin: bin.into(),
            ..TunnelConfig::default()
        })
    }

    fn http_url(&self, port: u16) -> String {
        http_url_for_bind(&self.access_bind, port)
    }

    /// Start or reuse a frontend for `share_id`. Returns the bound loopback port.
    pub async fn ensure(&self, share_id: &str) -> Result<u16, String> {
        let trimmed = share_id.trim();
        if trimmed.is_empty() {
            return Err("empty share id".into());
        }
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.by_share.get_mut(trimmed) {
            if frontend_alive(existing) {
                return Ok(existing.port);
            }
            let _ = inner.by_share.remove(trimmed);
        }
        let bound = match self.kind {
            FrontendKind::Loopback => bind_loopback(&self.access_bind).await?,
            FrontendKind::Zrok => {
                maybe_enable(
                    &self.zrok_bin,
                    self.api_endpoint.as_deref(),
                    &self.enable_token_env,
                    &mut inner.enabled,
                )
                .await?;
                spawn_zrok(
                    &self.zrok_bin,
                    self.api_endpoint.as_deref(),
                    &self.access_bind,
                    trimmed,
                )
                .await?
            }
        };
        let port = bound.port;
        inner.by_share.insert(trimmed.to_string(), bound);
        Ok(port)
    }

    /// Re-bind enrolled FleetState shares after a router restart.
    pub async fn restore_fleet(
        &self,
        fleet_state: &FleetState,
        registry: &Registry,
    ) -> Result<(), String> {
        let data = fleet_state
            .load_async()
            .await
            .map_err(|err| err.to_string())?;
        for (node_id, entry) in data {
            if entry.tunnel_backend.as_deref() != Some("zrok") {
                continue;
            }
            let Some(ollama) = entry
                .ollama_share_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(agent) = entry
                .agent_share_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let ollama_port = match self.ensure(ollama).await {
                Ok(port) => port,
                Err(err) => {
                    tracing::warn!(node_id = %node_id, error = %err, "enroll restore ollama frontend failed");
                    continue;
                }
            };
            let agent_port = match self.ensure(agent).await {
                Ok(port) => port,
                Err(err) => {
                    tracing::warn!(node_id = %node_id, error = %err, "enroll restore agent frontend failed");
                    continue;
                }
            };
            let url = self.http_url(ollama_port);
            let capacity_url = self.http_url(agent_port);
            if let Err(err) = fleet_state
                .persist_enroll_async(
                    &node_id,
                    ollama_router_core::fleet::EnrollPersist {
                        url: &url,
                        capacity_url: &capacity_url,
                        ollama_share_id: ollama,
                        agent_share_id: agent,
                    },
                )
                .await
            {
                tracing::warn!(node_id = %node_id, error = %err, "enroll restore persist failed");
                continue;
            }
            if let Ok(id) = NodeId::parse(&node_id) {
                let _ = registry.set_node_url(&id, &url);
                let _ = registry.set_capacity_url(&id, &capacity_url);
            }
            tracing::info!(
                node_id = %node_id,
                ollama_port,
                agent_port,
                tunnel_backend = "zrok",
                "enroll frontend restored"
            );
        }
        Ok(())
    }
}

fn frontend_alive(bound: &mut BoundFrontend) -> bool {
    if let Some(child) = bound.child.as_mut() {
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) | Err(_) => false,
        }
    } else if let Some(task) = &bound.serve {
        !task.is_finished()
    } else {
        bound._hold.is_some()
    }
}

async fn bind_loopback(access_bind: &str) -> Result<BoundFrontend, String> {
    let listener = TcpListener::bind(socket_addr_for_bind(access_bind, 0))
        .await
        .map_err(|err| format!("bind loopback: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("loopback local_addr: {err}"))?
        .port();
    let serve = tokio::spawn(serve_mock_tags(listener));
    Ok(BoundFrontend {
        port,
        _hold: None,
        child: None,
        serve: Some(serve),
    })
}

/// Test-only stand-in for `zrok access private`: answer GET /api/tags so enroll
/// loopback URLs are probeable. Production uses the zrok child, not this.
async fn serve_mock_tags(listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(async move {
            let _ = respond_mock_tags(stream).await;
        });
    }
}

async fn respond_mock_tags(mut stream: tokio::net::TcpStream) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; 1024];
    let _n = stream.read(&mut buf).await?;
    let body = br#"{"models":[]}"#;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

async fn maybe_enable(
    bin: &str,
    api_endpoint: Option<&str>,
    enable_token_env: &str,
    enabled: &mut bool,
) -> Result<(), String> {
    if *enabled {
        return Ok(());
    }
    let token = std::env::var(enable_token_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(token) = token else {
        *enabled = true;
        return Ok(());
    };
    if token.contains('\n') || token.contains('\r') || token.contains('\0') {
        return Err("enable token must be a single line".into());
    }
    let mut cmd = Command::new(bin);
    cmd.args(["enable", &token])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_zrok_env(&mut cmd, api_endpoint);
    let status = cmd
        .status()
        .await
        .map_err(|_| "zrok enable failed to start".to_string())?;
    if !status.success() {
        tracing::warn!("zrok enable failed or already enabled");
    }
    *enabled = true;
    Ok(())
}

async fn spawn_zrok(
    bin: &str,
    api_endpoint: Option<&str>,
    access_bind: &str,
    share_id: &str,
) -> Result<BoundFrontend, String> {
    let probe = TcpListener::bind(socket_addr_for_bind(access_bind, 0))
        .await
        .map_err(|err| format!("allocate port: {err}"))?;
    let port = probe
        .local_addr()
        .map_err(|err| format!("allocated port: {err}"))?
        .port();
    drop(probe);
    let bind = socket_addr_for_bind(access_bind, port);
    let args = access_private_args(share_id, &bind);
    let mut cmd = Command::new(bin);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_zrok_env(&mut cmd, api_endpoint);
    let mut child = cmd
        .spawn()
        .map_err(|_| "zrok access failed to start".to_string())?;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    match child.try_wait() {
        Ok(Some(_)) => return Err("zrok access exited".into()),
        Ok(None) => {}
        Err(_) => return Err("zrok access status".into()),
    }
    Ok(BoundFrontend {
        port,
        _hold: None,
        child: Some(child),
        serve: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_args_are_private_loopback_and_headless() {
        let args = access_private_args("token-id", "127.0.0.1:41990");
        assert_eq!(args[0], "access");
        assert_eq!(args[1], "private");
        assert!(args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--bindAddress".to_string()));
        assert!(args.contains(&"127.0.0.1:41990".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("token-id"));
    }

    #[test]
    fn process_env_sets_api_endpoint_only() {
        let env = zrok_process_env(Some("http://127.0.0.1:18080"));
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "ZROK_API_ENDPOINT");
        assert_eq!(env[0].1, "http://127.0.0.1:18080");
        assert!(zrok_process_env(Some("")).is_empty());
        assert!(zrok_process_env(None).is_empty());
    }

    #[tokio::test]
    async fn loopback_frontend_answers_tags() {
        let fronts = TunnelFrontends::loopback();
        let port = fronts.ensure("share-a").await.expect("bind");
        let url = format!("http://127.0.0.1:{port}/api/tags");
        let resp = reqwest::Client::new().get(&url).send().await.expect("tags");
        assert!(resp.status().is_success());
        let body = resp.text().await.expect("body");
        assert!(body.contains("models"), "{body}");
    }

    #[tokio::test]
    async fn loopback_reuses_port_for_same_share() {
        let fronts = TunnelFrontends::loopback();
        let a = fronts.ensure("share-a").await.expect("bind");
        let again = fronts.ensure("share-a").await.expect("reuse");
        assert_eq!(a, again);
        let b = fronts.ensure("share-b").await.expect("other");
        assert_ne!(a, b);
    }
}
