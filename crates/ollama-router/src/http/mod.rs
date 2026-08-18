//! Axum app: `/healthz`, `/readyz`, `/metrics`, aggregated tags and `/v1/models`, OpenAI inference, proxy fallback.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use ollama_router_core::cloud::{DemandScale, NoopDemandScale};
use ollama_router_core::config::RouterConfig;
use ollama_router_core::fleet::{FleetState, Registry};
use ollama_router_core::jobs::PullOrchestrator;
use ollama_router_core::routing::{looks_like_embedding, RoutingError, DEFAULT_EMBED_MARKERS};
use ollama_router_runpod::RunpodManager;
use ollama_router_verda::VerdaManager;
use rust_embed::RustEmbed;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::{MakeSpan, TraceLayer};
use tracing::Span;

use crate::proxy;
use crate::tunnel::TunnelFrontends;

mod admin;
pub mod metrics;

pub use metrics::Metrics;

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
    pub verda: Option<VerdaManager>,
    pub runpod: Option<RunpodManager>,
    pub metrics: Arc<Metrics>,
    pub tunnels: TunnelFrontends,
    /// Runtime kill switch: skip cloud create until resume or process restart.
    pub cloud_halted: Arc<AtomicBool>,
}

impl AppState {
    /// Production wiring: SQLite orchestrator, no demand scale-up.
    pub fn from_config(config: RouterConfig) -> anyhow::Result<Self> {
        Self::from_config_with_shutdown(config, CancellationToken::new())
    }

    /// Same as [`Self::from_config`], sharing the process shutdown token.
    pub fn from_config_with_shutdown(
        config: RouterConfig,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Self> {
        let client = build_upstream_client(&config)?;
        let pool = Arc::new(Semaphore::new(config.upstream.max_connections as usize));
        let config = Arc::new(config);
        let registry = Arc::new(Registry::new(&config));
        let metrics = Arc::new(Metrics::new()?);
        let orchestrator = Arc::new(PullOrchestrator::with_shutdown(
            config.clone(),
            client.clone(),
            Some(registry.clone()),
            shutdown,
        )?);
        orchestrator.set_observer(metrics.clone());
        let admin_token = std::env::var("OLLAMA_ROUTER_ADMIN_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let fleet_state = Arc::new(FleetState::new(&config.state_path));
        let tunnels = TunnelFrontends::from_config(&config.tunnel);
        let cloud_halted = Arc::new(AtomicBool::new(false));
        Ok(Self {
            config,
            registry,
            client,
            orchestrator,
            demand: Arc::new(GatedDemand {
                inner: Arc::new(NoopDemandScale),
                halted: cloud_halted.clone(),
            }),
            pool,
            tie_break: Arc::new(AtomicU64::new(0)),
            admin_token,
            fleet_state,
            verda: None,
            runpod: None,
            metrics,
            tunnels,
            cloud_halted,
        })
    }

    /// Replace demand scale-up, keeping the runtime cloud halt gate.
    pub fn set_cloud_demand(&mut self, inner: Arc<dyn DemandScale>) {
        self.demand = GatedDemand::wrap(inner, self.cloud_halted.clone());
    }
}

/// Demand fan-out that no-ops while [`AppState::cloud_halted`] is set.
pub(crate) struct GatedDemand {
    inner: Arc<dyn DemandScale>,
    halted: Arc<AtomicBool>,
}

impl GatedDemand {
    pub(crate) fn wrap(
        inner: Arc<dyn DemandScale>,
        halted: Arc<AtomicBool>,
    ) -> Arc<dyn DemandScale> {
        Arc::new(Self { inner, halted })
    }
}

impl DemandScale for GatedDemand {
    fn request_scale_up(&self, reason: RoutingError) {
        if self.halted.load(Ordering::Relaxed) {
            tracing::info!(
                reason = reason.as_reason_code(),
                "cloud_halted_scale_up_skipped"
            );
            return;
        }
        self.inner.request_scale_up(reason);
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
    let healthy: Vec<_> = snap
        .iter()
        .filter(|n| n.healthy && !n.draining && !n.cordoned)
        .collect();
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
    let default_max = state.config.policy.default_max_inflight;
    if !healthy.iter().any(|n| !n.is_saturated(default_max)) {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ready": false, "reason": "all nodes saturated"}),
        );
    }
    json_status(
        StatusCode::OK,
        json!({
            "ready": true,
            "healthy_nodes": healthy.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        }),
    )
}

async fn metrics(State(state): State<AppState>) -> Response {
    state.metrics.refresh_gauges(
        &state.registry,
        &state.fleet_state,
        Some(state.pool.available_permits()),
    );
    match state.metrics.encode_text() {
        Ok(body) => (
            [(header::CONTENT_TYPE, metrics::METRICS_CONTENT_TYPE)],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "metrics encode failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn proxy_route(State(state): State<AppState>, req: Request<axum::body::Body>) -> Response {
    proxy::handle(&state, req).await
}

async fn openai_model_route(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    proxy::openai_model_by_id(&state, &id, Instant::now())
}

async fn ui_index() -> Response {
    match UiAssets::get("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.data.into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Compile-time embed of the Vite `dist` tree. Rust never lists asset filenames.
#[derive(RustEmbed)]
#[folder = "ui/dist"]
struct UiAssets;

/// Tiny MIME map for the console (html/js/css only — no `mime_guess` crate).
fn ui_mime(path: &str) -> Option<&'static str> {
    match path.rsplit('.').next()? {
        "html" => Some("text/html; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        _ => None,
    }
}

fn ui_requested_static_asset(path: &str) -> bool {
    ui_mime(path).is_some()
}

async fn ui_asset(Path(path): Path<String>) -> Response {
    if let Some(file) = UiAssets::get(&path) {
        if let Some(content_type) = ui_mime(&path) {
            return (
                [(header::CONTENT_TYPE, content_type)],
                file.data.into_owned(),
            )
                .into_response();
        }
    }
    if ui_requested_static_asset(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    ui_index().await
}

/// Registered admin API `operationId` values (mirrors `make_app()` `/router/v1/*` routes).
/// Keep in sync with `site/openapi/openapi.yaml`.
pub const ADMIN_OPERATION_IDS: &[&str] = &[
    "cancelJob",
    "deleteModels",
    "drainNode",
    "enrollNode",
    "ensureModels",
    "getJob",
    "listJobs",
    "listModels",
    "listNodes",
    "putNode",
    "readiness",
    "readinessRecheck",
    "reload",
    "runpodDestroy",
    "runpodEnsure",
    "runpodStatus",
    "stats",
    "undrainNode",
    "verdaDestroy",
    "verdaEnsure",
    "verdaStatus",
    "cloudHalt",
    "cloudResume",
    "cloudStatus",
];

pub(crate) fn json_status(status: StatusCode, body: serde_json::Value) -> Response {
    let mut res = Json(body).into_response();
    *res.status_mut() = status;
    res
}

/// Build the router. `/healthz` and `/metrics` are unauthenticated; proxy is the fallback.
pub fn make_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/router/ui", get(ui_index))
        .route("/router/ui/", get(ui_index))
        .route("/router/ui/{*path}", get(ui_asset))
        .route("/api/tags", get(proxy_route))
        .route("/api/ps", get(proxy_route))
        .route("/api/version", get(proxy_route))
        .route("/api/generate", post(proxy_route))
        .route("/api/chat", post(proxy_route))
        .route("/api/embed", post(proxy_route))
        .route("/api/embeddings", post(proxy_route))
        .route("/api/stop", post(proxy_route))
        .route("/api/blobs/{*digest}", any(proxy_route))
        .route("/v1/models", get(proxy_route))
        .route("/v1/models/{*id}", get(openai_model_route))
        .route("/v1/chat/completions", post(proxy_route))
        .route("/v1/completions", post(proxy_route))
        .route("/v1/embeddings", post(proxy_route))
        .route("/api/show", post(proxy_route))
        .route("/api/push", post(proxy_route))
        .route("/api/copy", post(proxy_route))
        .route("/api/create", post(proxy_route))
        .route("/api/pull", post(proxy_route))
        .route("/api/delete", delete(proxy_route))
        .route(
            "/router/v1/nodes",
            get(admin::list_nodes).put(admin::put_node),
        )
        .route("/router/v1/nodes/{id}/drain", post(admin::drain_node))
        .route("/router/v1/nodes/{id}/undrain", post(admin::undrain_node))
        .route("/router/v1/readiness", get(admin::readiness))
        .route("/router/v1/readiness/recheck", post(admin::recheck))
        .route("/router/v1/models", get(admin::list_models))
        .route("/router/v1/models/ensure", post(admin::ensure_models))
        .route("/router/v1/models/delete", post(admin::delete_models))
        .route("/router/v1/jobs", get(admin::list_jobs))
        .route("/router/v1/jobs/{id}", get(admin::get_job))
        .route("/router/v1/jobs/{id}/cancel", post(admin::cancel_job))
        .route("/router/v1/stats", get(admin::stats))
        .route("/router/v1/reload", post(admin::reload))
        .route("/router/v1/nodes/enroll", post(admin::enroll_node))
        .route("/router/v1/verda/status", get(admin::verda_status))
        .route("/router/v1/verda/ensure", post(admin::verda_ensure))
        .route("/router/v1/verda/destroy", post(admin::verda_destroy))
        .route("/router/v1/runpod/status", get(admin::runpod_status))
        .route("/router/v1/runpod/ensure", post(admin::runpod_ensure))
        .route("/router/v1/runpod/destroy", post(admin::runpod_destroy))
        .route("/router/v1/cloud/status", get(admin::cloud_status))
        .route("/router/v1/cloud/halt", post(admin::cloud_halt))
        .route("/router/v1/cloud/resume", post(admin::cloud_resume))
        // Wrong method on a registered path must reach the proxy so envelopes
        // stay Ollama/OpenAI-shaped (Axum's default 405 would skip `.fallback`).
        .method_not_allowed_fallback(proxy_route)
        .fallback(proxy_route)
        .with_state(state)
        .layer(TraceLayer::new_for_http().make_span_with(RequestIdMakeSpan))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
        ]))
}

#[derive(Clone, Copy, Debug)]
struct RequestIdMakeSpan;

impl<B> MakeSpan<B> for RequestIdMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .and_then(|id| id.header_value().to_str().ok())
            .unwrap_or("-");
        tracing::info_span!(
            "http",
            method = %request.method(),
            uri = %request.uri(),
            request_id = %request_id,
        )
    }
}

#[cfg(test)]
mod gated_demand_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct CountingDemand {
        n: AtomicUsize,
    }

    impl DemandScale for CountingDemand {
        fn request_scale_up(&self, _reason: RoutingError) {
            self.n.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn halted_skips_inner_scale_up() {
        let inner = Arc::new(CountingDemand {
            n: AtomicUsize::new(0),
        });
        let halted = Arc::new(AtomicBool::new(true));
        let gated = GatedDemand {
            inner: inner.clone(),
            halted,
        };
        gated.request_scale_up(RoutingError::NoHealthy);
        assert_eq!(inner.n.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn resume_forwards_inner_scale_up() {
        let inner = Arc::new(CountingDemand {
            n: AtomicUsize::new(0),
        });
        let halted = Arc::new(AtomicBool::new(false));
        let gated = GatedDemand::wrap(inner.clone(), halted);
        gated.request_scale_up(RoutingError::Saturated);
        assert_eq!(inner.n.load(Ordering::Relaxed), 1);
    }
}
