//! Axum 0.8 app: `/healthz`, `/v1/capacity`, `/v1/pressure`, `/v1/status`, `/metrics`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ollama_capacity_types::{CapacityReport, PressureEnvelope};
use serde::Serialize;
use sysinfo::System;
use tokio::sync::RwLock;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::collect::{CollectError, Snapshot, StatusPayload};
use crate::config::AgentConfig;
use crate::listen::{format_host_port, resolve_bind, AddrSource, HostAddrs};
use crate::metrics::{AgentMetrics, METRICS_CONTENT_TYPE};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const COLLECT_TTL: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct CachedSnapshot {
    pub snap: crate::collect::Snapshot,
    pub at: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AgentConfig>,
    pub ollama_listen: String,
    pub metrics: Arc<AgentMetrics>,
    pub last: Arc<RwLock<Option<CachedSnapshot>>>,
    pub cpu_usage_pct: Arc<std::sync::RwLock<Option<f64>>>,
    /// Unit-test hook (crate tests only). Integration tests compile the lib without this field.
    #[cfg(test)]
    pub force_collect: Option<Result<Snapshot, CollectError>>,
}

fn require_token(expected: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(token) = expected.filter(|t| !t.is_empty()) else {
        return true;
    };
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let bearer = auth.strip_prefix("Bearer ").unwrap_or(auth);
    bearer == token
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

async fn collect_once(state: &AppState) -> Result<Snapshot, CollectError> {
    #[cfg(test)]
    if let Some(forced) = state.force_collect.clone() {
        return forced;
    }
    let cpu = state.cpu_usage_pct.read().ok().and_then(|slot| *slot);
    crate::collect::collect_live(&state.config, &state.ollama_listen, cpu).await
}

/// Fresh collect, or last cached inventory on join/cancel. Never invents empty GPUs.
async fn snapshot(state: &AppState) -> Result<Snapshot, StatusCode> {
    {
        let guard = state.last.read().await;
        if let Some(cached) = guard.as_ref() {
            if cached.at.elapsed() < COLLECT_TTL {
                return Ok(cached.snap.clone());
            }
        }
    }
    match collect_once(state).await {
        Ok(snap) => {
            state.metrics.observe(&snap);
            let mut guard = state.last.write().await;
            *guard = Some(CachedSnapshot {
                snap: snap.clone(),
                at: Instant::now(),
            });
            Ok(snap)
        }
        Err(err) => {
            tracing::warn!(error = %err, "collect failed");
            let guard = state.last.read().await;
            if let Some(cached) = guard.as_ref() {
                return Ok(cached.snap.clone());
            }
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn capacity(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CapacityReport>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await?.report))
}

async fn pressure(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PressureEnvelope>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await?.envelope))
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusPayload>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await?.status))
}

async fn metrics(State(state): State<AppState>) -> Response {
    let _ = snapshot(&state).await;
    match state.metrics.encode_text() {
        Ok(body) => ([(header::CONTENT_TYPE, METRICS_CONTENT_TYPE)], body).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub fn make_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/capacity", get(capacity))
        .route("/v1/pressure", get(pressure))
        .route("/v1/status", get(status))
        .route("/metrics", get(metrics))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state)
}

fn spawn_cpu_sampler(
    slot: Arc<std::sync::RwLock<Option<f64>>>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let sys = Arc::new(std::sync::Mutex::new(System::new()));
        if shutdown.is_cancelled() {
            return;
        }
        {
            let sys = Arc::clone(&sys);
            let _ = tokio::task::spawn_blocking(move || {
                sys.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .refresh_cpu_all();
            })
            .await;
        }
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            if shutdown.is_cancelled() {
                return;
            }
            let sys = Arc::clone(&sys);
            let pct = match tokio::task::spawn_blocking(move || {
                let mut guard = sys
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.refresh_cpu_all();
                f64::from(guard.global_cpu_usage())
            })
            .await
            {
                Ok(pct) => pct,
                Err(_) => continue,
            };
            if let Ok(mut guard) = slot.write() {
                *guard = Some(pct);
            }
        }
    })
}

pub fn prepare_serve(
    config: Option<&std::path::Path>,
    host: Option<String>,
    port: Option<u16>,
) -> anyhow::Result<(AgentConfig, std::net::SocketAddr, String)> {
    let mut cfg = AgentConfig::load(config).context("load config")?;
    if let Some(h) = host {
        cfg.listen = crate::config::BindSpec::Address(h);
    }
    if let Some(p) = port {
        cfg.port = p;
    }
    let token_set = cfg.bearer_token().is_some();
    let addrs = HostAddrs;
    let addrs: &dyn AddrSource = &addrs;
    let agent_ip = resolve_bind(&cfg.listen, addrs, token_set)?;
    let ollama_ip = resolve_bind(&cfg.ollama.listen, addrs, token_set)?;
    let bind = std::net::SocketAddr::new(agent_ip, cfg.port);
    let ollama_listen = format_host_port(ollama_ip, 11434);
    Ok((cfg, bind, ollama_listen))
}

pub async fn serve(
    config: AgentConfig,
    bind: std::net::SocketAddr,
    ollama_listen: String,
) -> anyhow::Result<()> {
    serve_with_shutdown(config, bind, ollama_listen, shutdown_signal()).await
}

pub async fn serve_with_shutdown(
    config: AgentConfig,
    bind: std::net::SocketAddr,
    ollama_listen: String,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    if matches!(bind.ip(), std::net::IpAddr::V4(v) if v.is_unspecified())
        && config.bearer_token().is_none()
    {
        anyhow::bail!("listen all requires a bearer token");
    }
    let metrics = Arc::new(AgentMetrics::new()?);
    let cpu_usage_pct = Arc::new(std::sync::RwLock::new(None));
    let tasks = CancellationToken::new();
    let cpu_handle = spawn_cpu_sampler(Arc::clone(&cpu_usage_pct), tasks.clone());
    let state = AppState {
        config: Arc::new(config.clone()),
        ollama_listen,
        metrics,
        last: Arc::new(RwLock::new(None)),
        cpu_usage_pct,
        #[cfg(test)]
        force_collect: None,
    };
    let app = make_app(state.clone());
    let enroll_handle = crate::register::spawn_if_configured(state, tasks.clone());
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "ollama-node-agent listening");
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown({
            let tasks = tasks.clone();
            async move {
                shutdown.await;
                tasks.cancel();
            }
        })
        .await;
    tasks.cancel();
    cpu_handle.abort();
    if let Some(handle) = enroll_handle {
        handle.abort();
    }
    serve_result?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "ctrl_c listener failed");
        }
    };
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            () = ctrl_c => {}
            () = async {
                if let Some(ref mut stream) = sigterm {
                    stream.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{collect_from_parts, CollectParts, GpuInventory};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use ollama_capacity_types::{GpuBackend, GpuDetail};
    use tower::ServiceExt;

    fn gpu_snapshot_full_vram() -> Snapshot {
        let gpu = crate::collect::inventory_from_details(vec![GpuDetail {
            index: 0,
            name: "NVIDIA GeForce RTX 4090".into(),
            vram_total_gb: 16.0,
            vram_used_gb: 16.0,
            vram_free_gb: 0.0,
            vram_free_known: Some(true),
            vram_used_known: Some(true),
            ..GpuDetail::default()
        }]);
        assert_eq!(gpu.gpus, 1);
        assert!(gpu.vram_free_known);
        collect_from_parts(
            &AgentConfig::default(),
            CollectParts {
                gpu,
                backend: GpuBackend::Cuda,
                ollama_listen: "127.0.0.1:11434".into(),
                ..CollectParts::default()
            },
        )
    }

    fn app_with_cache(
        snap: Snapshot,
        age: Duration,
        force: Option<Result<Snapshot, CollectError>>,
    ) -> AppState {
        AppState {
            config: Arc::new(AgentConfig::default()),
            ollama_listen: "127.0.0.1:11434".into(),
            metrics: Arc::new(AgentMetrics::new().expect("metrics")),
            last: Arc::new(RwLock::new(Some(CachedSnapshot {
                snap,
                at: Instant::now()
                    .checked_sub(age)
                    .expect("cache age within Instant"),
            }))),
            cpu_usage_pct: Arc::new(std::sync::RwLock::new(None)),
            force_collect: force,
        }
    }

    #[tokio::test]
    async fn collect_join_error_keeps_cached_gpus() {
        let snap = gpu_snapshot_full_vram();
        assert_eq!(snap.report.gpus, 1);
        assert!((snap.report.vram_free_gb - 0.0).abs() < 1e-12);
        assert_eq!(snap.report.vram_free_known, Some(true));
        assert!(snap.report.vram_free_is_known());

        let empty = collect_from_parts(&AgentConfig::default(), CollectParts::default());
        assert_eq!(empty.report.gpus, 0);
        assert!(!empty.report.vram_free_is_known());

        let state = app_with_cache(
            snap,
            COLLECT_TTL + Duration::from_secs(1),
            Some(Err(CollectError::Join("cancelled".into()))),
        );
        let got = snapshot(&state).await.expect("cached snapshot");
        assert_eq!(got.report.gpus, 1);
        assert_eq!(got.report.gpu_names, vec!["NVIDIA GeForce RTX 4090"]);
        assert!((got.report.vram_gb - 16.0).abs() < 1e-9);
        assert!((got.report.vram_free_gb - 0.0).abs() < 1e-12);
        assert_eq!(got.report.vram_free_known, Some(true));
        assert!(got.report.vram_free_is_known());

        let response = make_app(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let report: CapacityReport = serde_json::from_slice(&body).unwrap();
        assert_eq!(report.gpus, 1);
        assert_eq!(report.vram_free_known, Some(true));
        assert!((report.vram_free_gb - 0.0).abs() < 1e-12);
    }

    #[tokio::test]
    async fn collect_join_error_without_cache_is_unavailable() {
        let state = AppState {
            config: Arc::new(AgentConfig::default()),
            ollama_listen: "127.0.0.1:11434".into(),
            metrics: Arc::new(AgentMetrics::new().expect("metrics")),
            last: Arc::new(RwLock::new(None)),
            cpu_usage_pct: Arc::new(std::sync::RwLock::new(None)),
            force_collect: Some(Err(CollectError::Join("panic".into()))),
        };
        assert!(
            matches!(snapshot(&state).await, Err(StatusCode::SERVICE_UNAVAILABLE)),
            "empty cache must not invent inventory"
        );
        let response = make_app(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/capacity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn serve_with_shutdown_ready_future_returns() {
        let cfg = AgentConfig::default();
        let bind = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        serve_with_shutdown(cfg, bind, "127.0.0.1:11434".into(), std::future::ready(()))
            .await
            .expect("serve returned");
    }

    #[tokio::test]
    async fn cpu_sampler_ends_when_shutdown_cancelled() {
        let slot = Arc::new(std::sync::RwLock::new(None));
        let token = CancellationToken::new();
        let handle = spawn_cpu_sampler(slot, token.clone());
        token.cancel();
        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("sampler joined")
            .ok();
    }

    #[test]
    fn unused_gpu_inventory_default_is_unknown_free() {
        let gpu = GpuInventory::default();
        assert_eq!(gpu.gpus, 0);
        assert!((gpu.vram_free_gb - 0.0).abs() < 1e-12);
        assert!(!gpu.vram_free_known);
    }
}
