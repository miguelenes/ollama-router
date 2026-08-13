//! Admin API: `/router/v1/models/*`, jobs, provision, and `/router/v1/verda/*`.
//!
//! Bearer token comes from `OLLAMA_ROUTER_ADMIN_TOKEN` (captured on
//! [`AppState`] construction). Unset → 403. No Thunder / RunPod routes.

use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use ollama_router_core::jobs::{Job, OrchestratorError};
use ollama_router_core::provision::ProvisionOpts;
use ollama_router_core::routing::TargetSpec;

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
