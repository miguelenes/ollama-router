//! Admin API: `/router/v1/models/*`, jobs, nodes, stats, reload, provision, Verda.
//!
//! Bearer token comes from `OLLAMA_ROUTER_ADMIN_TOKEN` (captured on
//! [`AppState`] construction). Unset → 403. No Thunder / RunPod routes.

use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use ollama_router_core::fleet::{url_host_is_public_ipv4, NodeId};
use ollama_router_core::jobs::{Job, OrchestratorError};
use ollama_router_core::provision::ProvisionOpts;
use ollama_router_core::routing::{placement_eligible_node_ids, TargetSpec};

use super::{json_status, AppState};

#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NodesArg {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub struct ModelOpRequest {
    pub models: Vec<String>,
    #[serde(default)]
    nodes: Option<NodesArg>,
    #[serde(default)]
    pub wait: bool,
}

impl ModelOpRequest {
    fn spec(&self) -> Result<TargetSpec, OrchestratorError> {
        match &self.nodes {
            None => Ok(TargetSpec::Placement),
            Some(NodesArg::One(raw)) => TargetSpec::parse_one(Some(raw)).map_err(Into::into),
            Some(NodesArg::Many(raw)) => TargetSpec::parse(Some(raw)).map_err(Into::into),
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state
        .admin_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Some(json_status(
            StatusCode::FORBIDDEN,
            json!({"error": "admin API disabled: set OLLAMA_ROUTER_ADMIN_TOKEN"}),
        ));
    };
    let presented = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected_header = format!("Bearer {expected}");
    if !constant_time_eq(presented.as_bytes(), expected_header.as_bytes()) {
        return Some(json_status(
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid admin token"}),
        ));
    }
    None
}

fn map_orch_err(err: OrchestratorError) -> Response {
    match err {
        OrchestratorError::EmptyModels
        | OrchestratorError::UnknownNode(_)
        | OrchestratorError::NoPlacementTargets
        | OrchestratorError::NoTargetNodes => json_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": err.to_string()}),
        ),
        OrchestratorError::NotConfigured => json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": err.to_string()}),
        ),
        OrchestratorError::Other(_) => {
            json_status(StatusCode::BAD_GATEWAY, json!({"error": err.to_string()}))
        }
    }
}

fn accepted(job: &Job) -> Response {
    json_status(StatusCode::ACCEPTED, json!({"job_id": job.id, "job": job}))
}

async fn maybe_wait(state: &AppState, job: Job, wait: bool) -> Response {
    if !wait {
        return accepted(&job);
    }
    let timeout = std::time::Duration::from_secs_f64(state.config.ensure_wait_max_seconds);
    let (current, timed_out) = state.orchestrator.wait_job_timeout(&job.id, timeout).await;
    if timed_out {
        json_status(
            StatusCode::ACCEPTED,
            json!({
                "job_id": current.id,
                "job": current,
                "wait_timeout_seconds": state.config.ensure_wait_max_seconds,
            }),
        )
    } else {
        json_status(
            StatusCode::OK,
            json!({"job_id": current.id, "job": current}),
        )
    }
}

pub async fn ensure_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WaitQuery>,
    Json(body): Json<ModelOpRequest>,
) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let spec = match body.spec() {
        Ok(spec) => spec,
        Err(err) => return map_orch_err(err),
    };
    let job = match state
        .orchestrator
        .start_ensure(&body.models, spec, false, false)
    {
        Ok(job) => job,
        Err(err) => return map_orch_err(err),
    };
    maybe_wait(&state, job, query.wait || body.wait).await
}

pub async fn delete_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WaitQuery>,
    Json(body): Json<ModelOpRequest>,
) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let spec = match body.spec() {
        Ok(spec) => spec,
        Err(err) => return map_orch_err(err),
    };
    let job = match state.orchestrator.start_delete(&body.models, spec, false) {
        Ok(job) => job,
        Err(err) => return map_orch_err(err),
    };
    maybe_wait(&state, job, query.wait || body.wait).await
}

pub async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Ok(id) = ollama_router_core::jobs::JobId::parse(&id) else {
        return json_status(StatusCode::NOT_FOUND, json!({"error": "job not found"}));
    };
    match state.orchestrator.get_job(&id) {
        Some(job) => Json(job).into_response(),
        None => json_status(StatusCode::NOT_FOUND, json!({"error": "job not found"})),
    }
}

pub async fn list_jobs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    Json(json!({"jobs": state.orchestrator.list_jobs()})).into_response()
}

pub async fn list_nodes(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let mut nodes = state.registry.snapshot();
    nodes.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    let body: Vec<_> = nodes.iter().map(node_public_view).collect();
    Json(json!({"nodes": body})).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PutNodeRequest {
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

pub async fn put_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PutNodeRequest>,
) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Ok(id) = NodeId::parse(body.id.trim()) else {
        return json_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": "invalid node id"}),
        );
    };
    if let Some(url) = body.url.as_deref() {
        let trimmed = url.trim();
        if !trimmed.is_empty() && url_host_is_public_ipv4(trimmed) {
            return json_status(
                StatusCode::BAD_REQUEST,
                json!({"error": "refusing public IPv4 routing URL"}),
            );
        }
    }
    let existing = state.registry.get(&id);
    if existing.is_none() {
        state
            .registry
            .upsert_verda(ollama_router_core::config::NodeConfig {
                id: id.clone(),
                url: None,
                capacity_url: None,
                labels: body.labels.clone().unwrap_or_default(),
                static_capacity: ollama_router_core::config::Capacity::default(),
                max_inflight: None,
                ssh: None,
                provision: None,
            });
    }
    if let Some(labels) = body.labels {
        state.registry.set_node_labels(&id, labels);
    }
    if let Some(url) = body.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        if let Err(err) = state.registry.set_node_url(&id, url) {
            return json_status(StatusCode::BAD_REQUEST, json!({"error": err}));
        }
        if let Err(err) = state.fleet_state.persist_url(id.as_str(), url, None) {
            tracing::warn!(node_id = %id, error = %err, "put_node persist_url failed");
        }
    }
    let Some(snap) = state.registry.get(&id) else {
        return json_status(StatusCode::NOT_FOUND, json!({"error": "node not found"}));
    };
    Json(json!({"node": node_public_view(&snap)})).into_response()
}

pub async fn list_models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let snap = state.registry.snapshot();
    let policy = &state.config.policy;
    let tiers = state.config.effective_model_tiers();
    let mut desired: Vec<String> = tiers
        .iter()
        .flat_map(|t| t.models.iter())
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty())
        .collect();
    desired.sort();
    desired.dedup();
    let mut observed: Vec<String> = snap.iter().flat_map(|n| n.models.iter().cloned()).collect();
    observed.sort();
    observed.dedup();
    let mut catalog = desired.clone();
    catalog.extend(observed);
    catalog.sort();
    catalog.dedup();
    let mut matrix = serde_json::Map::new();
    for model in &catalog {
        let mut row = serde_json::Map::new();
        for node in &snap {
            row.insert(node.id.as_str().to_string(), json!(node.has_model(model)));
        }
        matrix.insert(model.clone(), json!(row));
    }
    let mut placement = serde_json::Map::new();
    let mut eligible_including_unhealthy = serde_json::Map::new();
    for model in &desired {
        let ids: Vec<String> = placement_eligible_node_ids(&snap, model, policy, false, false)
            .into_iter()
            .map(|id| id.as_str().to_string())
            .collect();
        let all: Vec<String> = placement_eligible_node_ids(&snap, model, policy, true, false)
            .into_iter()
            .map(|id| id.as_str().to_string())
            .collect();
        placement.insert(model.clone(), json!(ids));
        eligible_including_unhealthy.insert(model.clone(), json!(all));
    }
    let mut node_tier_models = serde_json::Map::new();
    for node in &snap {
        node_tier_models.insert(
            node.id.as_str().to_string(),
            json!(state.config.tier_models_for_vram(node.vram_gb())),
        );
    }
    Json(json!({
        "desired_models": desired,
        "tiers": tiers.iter().map(|t| json!({
            "models": t.models,
            "min_vram_gb": t.min_vram_gb,
        })).collect::<Vec<_>>(),
        "node_tier_models": node_tier_models,
        "nodes": snap.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        "matrix": matrix,
        "placement": placement,
        "eligible_including_unhealthy": eligible_including_unhealthy,
    }))
    .into_response()
}

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    Json(
        state
            .metrics
            .stats_json(&state.registry, &state.fleet_state),
    )
    .into_response()
}

pub async fn reload(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    match crate::health::reload_permanent_inventory(&state) {
        Ok(()) => json_status(StatusCode::OK, json!({"ok": true})),
        Err(err) => json_status(StatusCode::BAD_GATEWAY, json!({"error": err.to_string()})),
    }
}

fn node_public_view(node: &ollama_router_core::fleet::NodeSnapshot) -> serde_json::Value {
    let mut models: Vec<_> = node.models.iter().cloned().collect();
    models.sort();
    json!({
        "id": node.id.as_str(),
        "url": node.url,
        "origin": node.origin.as_str(),
        "healthy": node.healthy,
        "labels": node.labels,
        "inflight": node.inflight,
        "models": models,
        "capacity": node.capacity,
        "pressure": node.pressure_level.as_str(),
        "vram_free_gb": node.vram_free_gb,
    })
}

#[derive(Debug, Deserialize)]
pub struct ProvisionRequest {
    #[serde(default)]
    pub nodes: Option<Vec<String>>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub force: bool,
}

pub async fn provision_nodes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ProvisionRequest>,
) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Some(provisioner) = state.provisioner.as_ref() else {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "provisioner not available"}),
        );
    };
    let opts = ProvisionOpts {
        dry_run: body.dry_run,
        force: body.force,
        wait_for_public_ssh: false,
    };
    let result = provisioner
        .provision_many(body.nodes.as_deref(), opts)
        .await;
    match result {
        Err(err) => json_status(StatusCode::UNPROCESSABLE_ENTITY, json!({"error": err})),
        Ok(results) => {
            let failed = results.iter().any(|r| r.status.as_str() == "fail");
            let payload = json!({
                "results": results.iter().map(|r| json!({
                    "node_id": r.node_id.as_str(),
                    "status": r.status.as_str(),
                    "detail": r.detail,
                    "tailscale_ip": r.tailscale_ip,
                    "phase": r.phase,
                })).collect::<Vec<_>>(),
            });
            json_status(
                if failed {
                    StatusCode::MULTI_STATUS
                } else {
                    StatusCode::OK
                },
                payload,
            )
        }
    }
}

fn require_verda(state: &AppState) -> Option<&ollama_router_verda::VerdaManager> {
    state.verda.as_ref()
}

pub async fn verda_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Some(mgr) = require_verda(&state) else {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "Verda Cloud not enabled (verda.enabled=false)"}),
        );
    };
    Json(mgr.status().await).into_response()
}

pub async fn verda_ensure(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Some(mgr) = require_verda(&state) else {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "Verda Cloud not enabled (verda.enabled=false)"}),
        );
    };
    match mgr.ensure(true).await {
        Ok(body) => json_status(StatusCode::OK, body),
        Err(err) => json_status(
            StatusCode::BAD_GATEWAY,
            json!({"error": err.to_string().chars().take(200).collect::<String>()}),
        ),
    }
}

pub async fn verda_destroy(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(resp) = require_admin(&state, &headers) {
        return resp;
    }
    let Some(mgr) = require_verda(&state) else {
        return json_status(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "Verda Cloud not enabled (verda.enabled=false)"}),
        );
    };
    let body = mgr.destroy_all_owned().await;
    let failed = body
        .get("failed")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    json_status(
        if failed {
            StatusCode::MULTI_STATUS
        } else {
            StatusCode::OK
        },
        body,
    )
}
