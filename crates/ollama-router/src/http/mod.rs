//! Axum app: `/healthz`, `/readyz`, `/metrics` stub, aggregated tags, proxy fallback.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use ollama_router_core::cloud::{DemandScale, NoopDemandScale};
use ollama_router_core::config::RouterConfig;
use ollama_router_core::fleet::{FleetState, Registry};
use ollama_router_core::jobs::PullOrchestrator;
use ollama_router_core::routing::{looks_like_embedding, DEFAULT_EMBED_MARKERS};
use ollama_router_verda::VerdaManager;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::provision::ProvisionOrchestrator;
use crate::proxy;

mod admin;

/// Shared proxy / admin state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RouterConfig>,
    pub registry: Arc<Registry>,
    pub client: reqwest::Client,
    pub orchestrator: Arc<PullOrchestrator>,
    pub demand: Arc<dyn DemandScale>,
    pub pool: Arc<Semaphore>,
    pub tie_break: Arc<AtomicU64>,
    /// Captured from `OLLAMA_ROUTER_ADMIN_TOKEN` (never YAML). Unset disables admin.
    pub admin_token: Option<String>,
    pub fleet_state: Arc<FleetState>,
    pub provisioner: Option<Arc<ProvisionOrchestrator>>,
    pub verda: Option<VerdaManager>,
}

impl AppState {
    /// Production wiring: SQLite orchestrator, no demand scale-up.
    pub fn from_config(config: RouterConfig) -> anyhow::Result<Self> {
        let client = build_upstream_client(&config)?;
        let pool = Arc::new(Semaphore::new(config.upstream.max_connections as usize));
        let config = Arc::new(config);
        let registry = Arc::new(Registry::new(&config));
        let orchestrator = Arc::new(PullOrchestrator::new(
            config.clone(),
            client.clone(),
            Some(registry.clone()),
        )?);
        let admin_token = std::env::var("OLLAMA_ROUTER_ADMIN_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let fleet_state = Arc::new(FleetState::new(&config.state_path));
        let provisioner = Some(Arc::new(ProvisionOrchestrator::new(
            config.clone(),
            client.clone(),
            Some(registry.clone()),
            Some(fleet_state.clone()),
        )));
        Ok(Self {
            config,
            registry,
            client,
            orchestrator,
            demand: Arc::new(NoopDemandScale),
            pool,
            tie_break: Arc::new(AtomicU64::new(0)),
            admin_token,
            fleet_state,
            provisioner,
            verda: None,
        })
    }
}

/// rustls-only reqwest client with configured pool keepalive.
pub fn build_upstream_client(config: &RouterConfig) -> anyhow::Result<reqwest::Client> {
    let connect = Duration::from_secs_f64(config.timeouts.connect_seconds);
    Ok(reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(connect)
        .pool_max_idle_per_host(config.upstream.max_keepalive_connections as usize)
        .http1_only()
        .build()?)
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

async fn readyz(State(state): State<AppState>) -> Response {
    let snap = state.registry.snapshot();
    let healthy: Vec<_> = snap.iter().filter(|n| n.healthy && !n.draining).collect();
    if healthy.is_empty() {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ready": false, "reason": "no healthy nodes"}),
        );
    }
    if state.config.ready_requires_embedding_model {
        let wanted: Vec<String> = state
            .config
            .effective_model_tiers()
            .iter()
            .flat_map(|tier| tier.models.iter())
            .filter(|m| looks_like_embedding(m, DEFAULT_EMBED_MARKERS))
            .map(|m| m.trim().to_ascii_lowercase())
            .collect();
        if !wanted.is_empty() {
            let present = healthy
                .iter()
                .any(|n| wanted.iter().any(|m| n.has_model(m)));
            if !present {
                return json_status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"ready": false, "reason": "embedding model not on any healthy node"}),
                );
            }
        }
    }
    json_status(
        StatusCode::OK,
        json!({
            "ready": true,
            "healthy_nodes": healthy.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        }),
    )
}

async fn metrics() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        "# ollama-router metrics (not yet implemented)\n",
    )
}

async fn proxy_route(State(state): State<AppState>, req: Request<axum::body::Body>) -> Response {
    proxy::handle(&state, req).await
}

pub(crate) fn json_status(status: StatusCode, body: serde_json::Value) -> Response {
    let mut res = Json(body).into_response();
    *res.status_mut() = status;
    res
}

/// Build the router. `/healthz` is unauthenticated; proxy is the fallback.
pub fn make_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/api/tags", get(proxy_route))
        .route("/api/pull", post(proxy_route))
        .route("/api/delete", delete(proxy_route))
        .route("/router/v1/models/ensure", post(admin::ensure_models))
        .route("/router/v1/models/delete", post(admin::delete_models))
        .route("/router/v1/jobs/{id}", get(admin::get_job))
        .route("/router/v1/nodes/provision", post(admin::provision_nodes))
        .route("/router/v1/verda/status", get(admin::verda_status))
        .route("/router/v1/verda/ensure", post(admin::verda_ensure))
        .route("/router/v1/verda/destroy", post(admin::verda_destroy))
        .fallback(proxy_route)
        .with_state(state)
}
