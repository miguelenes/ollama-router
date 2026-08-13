//! Axum 0.8 app: `/healthz`, `/v1/capacity`, `/v1/pressure`, `/v1/status`, `/metrics`.

use std::sync::Arc;
use std::time::{Duration, Instant};

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

use crate::collect::StatusPayload;
use crate::config::AgentConfig;
use crate::metrics::{AgentMetrics, METRICS_CONTENT_TYPE};

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

async fn snapshot(state: &AppState) -> crate::collect::Snapshot {
    {
        let guard = state.last.read().await;
        if let Some(cached) = guard.as_ref() {
            if cached.at.elapsed() < COLLECT_TTL {
                return cached.snap.clone();
            }
        }
    }
    let mut guard = state.last.write().await;
    if let Some(cached) = guard.as_ref() {
        if cached.at.elapsed() < COLLECT_TTL {
            return cached.snap.clone();
        }
    }
    let cpu = state.cpu_usage_pct.read().ok().and_then(|slot| *slot);
    let snap = crate::collect::collect_live(&state.config, &state.ollama_listen, cpu).await;
    state.metrics.observe(&snap);
    *guard = Some(CachedSnapshot {
        snap: snap.clone(),
        at: Instant::now(),
    });
    snap
}

async fn capacity(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CapacityReport>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await.report))
}

async fn pressure(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PressureEnvelope>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await.envelope))
}

async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusPayload>, StatusCode> {
    if !require_token(state.config.bearer_token(), &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(snapshot(&state).await.status))
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

fn spawn_cpu_sampler(slot: Arc<std::sync::RwLock<Option<f64>>>) {
    tokio::spawn(async move {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            sys.refresh_cpu_all();
            let pct = f64::from(sys.global_cpu_usage());
            if let Ok(mut guard) = slot.write() {
                *guard = Some(pct);
            }
        }
    });
}

pub async fn serve(
    config: AgentConfig,
    bind: std::net::SocketAddr,
    ollama_listen: String,
) -> anyhow::Result<()> {
    if matches!(bind.ip(), std::net::IpAddr::V4(v) if v.is_unspecified())
        && config.bearer_token().is_none()
    {
        anyhow::bail!("listen all requires a bearer token");
    }
    let metrics = Arc::new(AgentMetrics::new()?);
    let cpu_usage_pct = Arc::new(std::sync::RwLock::new(None));
    spawn_cpu_sampler(Arc::clone(&cpu_usage_pct));
    let state = AppState {
        config: Arc::new(config.clone()),
        ollama_listen,
        metrics,
        last: Arc::new(RwLock::new(None)),
        cpu_usage_pct,
    };
    let app = make_app(state.clone());
    crate::register::spawn_if_configured(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "ollama-node-agent listening");
    axum::serve(listener, app).await?;
    Ok(())
}
