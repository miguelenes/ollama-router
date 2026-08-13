//! NDJSON streaming reverse proxy; `/api/embeddings` → `/api/embed`.

mod telemetry;

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    Method, Request, StatusCode,
};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::Stream;
use http_body_util::{BodyExt, Limited};
use ollama_router_core::cloud::DemandScale;
use ollama_router_core::config::{PolicyConfig, TimeoutsConfig};
use ollama_router_core::fleet::{NodeId, NodeSnapshot, Registry};
use ollama_router_core::jobs::{JobStatus, OrchestratorError};
use ollama_router_core::routing::{
    blocked_only_by_reservations, classify, estimate_request_ram_gb, estimate_request_vram_gb,
    rank_nodes, RequestClass, RoutingError,
};
use serde_json::{json, Value};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

pub use telemetry::{IncrementalCollector, MAX_INCOMPLETE_FRAME_BYTES};

use crate::http::AppState;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Catch-all Ollama-compatible reverse proxy.
pub async fn handle(state: &AppState, req: Request<Body>) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);

    if path.trim_end_matches('/') == "/api/tags" && method == Method::GET {
        return aggregated_tags(state);
    }

    let (parts, incoming_body) = req.into_parts();
    let incoming_headers = parts.headers;

    let mut model: Option<String> = None;
    let mut body = Bytes::new();
    if matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        match read_body_capped(
            &incoming_headers,
            incoming_body,
            state.config.policy.max_request_body_bytes,
        )
        .await
        {
            Ok(bytes) => {
                model = extract_model(&bytes);
                body = bytes;
            }
            Err(BodyCapError::InvalidContentLength) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    json!({"error": "ollama-router: invalid Content-Length"}),
                    None,
                );
            }
            Err(BodyCapError::TooLarge) => {
                return json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    json!({
                        "error": "ollama-router: request body exceeds configured limit",
                        "max_request_body_bytes": state.config.policy.max_request_body_bytes,
                    }),
                    None,
                );
            }
        }
    }

    if !state.config.policy.unsafe_single_node_mutate {
        let clean = path.trim_end_matches('/');
        if clean == "/api/pull" && method == Method::POST {
            return fleet_pull(state, model.as_deref()).await;
        }
        if clean == "/api/delete" && method == Method::DELETE {
            return fleet_delete(state, model.as_deref()).await;
        }
    }

    let request_class = classify(&path, model.as_deref(), &state.config.policy);
    proxy_ranked(
        state,
        method,
        &path,
        query.as_deref(),
        &incoming_headers,
        body,
        model,
        request_class,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn proxy_ranked(
    state: &AppState,
    method: Method,
    path: &str,
    query: Option<&str>,
    incoming_headers: &HeaderMap,
    body: Bytes,
    model: Option<String>,
    request_class: RequestClass,
) -> Response {
    let policy = &state.config.policy;
    let mut excluded: HashSet<NodeId> = HashSet::new();

    let mut outcome = rank(state, request_class, model.as_deref(), &excluded);
    if !outcome.ok()
        && outcome.reason == Some(RoutingError::Saturated)
        && policy.overload_wait_ms > 0
    {
        tracing::info!(
            path,
            request_class = %request_class,
            wait_ms = policy.overload_wait_ms,
            "overload_wait"
        );
        tokio::time::sleep(Duration::from_millis(u64::from(policy.overload_wait_ms))).await;
        outcome = rank(state, request_class, model.as_deref(), &excluded);
    }
    if !outcome.ok()
        && outcome.reason == Some(RoutingError::Capacity)
        && policy.admission_wait_ms > 0
        && blocked_only_by_reservations(
            &state.registry.snapshot(),
            request_class,
            model.as_deref(),
            policy,
        )
    {
        tracing::info!(
            path,
            request_class = %request_class,
            model = model.as_deref().unwrap_or(""),
            wait_ms = policy.admission_wait_ms,
            "admission_wait"
        );
        tokio::time::sleep(Duration::from_millis(u64::from(policy.admission_wait_ms))).await;
        outcome = rank(state, request_class, model.as_deref(), &excluded);
    }

    if !outcome.ok() {
        let reason = outcome.reason.unwrap_or(RoutingError::NoHealthy);
        tracing::warn!(
            path,
            request_class = %request_class,
            model = model.as_deref().unwrap_or(""),
            reason = reason.as_reason_code(),
            "route_rejected"
        );
        if reason.requests_demand_scale_up() {
            DemandScale::request_scale_up(state.demand.as_ref(), reason);
        }
        if reason == RoutingError::ModelMissing && model.is_some() && policy.auto_pull_on_miss {
            return auto_pull_miss(state, model.as_deref().unwrap_or(""), request_class).await;
        }
        return no_candidate_response(reason, model.as_deref(), request_class);
    }

    let mut ranked = outcome.ranked;
    let max_attempts = (policy.retry_max_attempts as usize)
        .min(ranked.len())
        .max(1);
    let mut last_error: Option<ForwardError> = None;
    for attempt in 0..max_attempts {
        let node = ranked[0].clone();
        excluded.insert(node.id.clone());
        let has_next = attempt + 1 < max_attempts;
        match forward_once(
            state,
            &method,
            path,
            query,
            incoming_headers,
            &body,
            &node,
            request_class,
            model.as_deref(),
        )
        .await
        {
            Ok(response) => return response,
            Err(err) => {
                match &err {
                    ForwardError::Overload { status } => {
                        state.registry.mark_request_overload(&node.id);
                        tracing::warn!(
                            node = %node.id,
                            path,
                            request_class = %request_class,
                            retry_reason = format!("status_{status}"),
                            attempt = attempt + 1,
                            max_attempts,
                            "upstream_retry"
                        );
                    }
                    ForwardError::Retryable { reason, message } => {
                        state.registry.mark_request_failure(&node.id);
                        tracing::warn!(
                            node = %node.id,
                            path,
                            request_class = %request_class,
                            retry_reason = *reason,
                            attempt = attempt + 1,
                            max_attempts,
                            error = %truncate(message, 200),
                            "upstream_retry"
                        );
                    }
                    ForwardError::Fatal { kind, message } => {
                        state.registry.mark_request_failure(&node.id);
                        return upstream_unavailable(kind, message);
                    }
                }
                last_error = Some(err);
                if has_next {
                    let reranked = rank(state, request_class, model.as_deref(), &excluded);
                    if !reranked.ok() {
                        break;
                    }
                    ranked = reranked.ranked;
                }
            }
        }
    }

    match last_error {
        Some(ForwardError::Overload { status }) => {
            DemandScale::request_scale_up(state.demand.as_ref(), RoutingError::Saturated);
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "error": format!(
                        "ollama-router: all attempted nodes are overloaded (upstream http {status}) [class: {request_class}, reason: {}]",
                        RoutingError::Saturated.as_reason_code()
                    )
                }),
                Some(30),
            )
        }
        Some(ForwardError::Retryable { reason, message }) => upstream_unavailable(reason, &message),
        Some(ForwardError::Fatal { kind, message }) => upstream_unavailable(&kind, &message),
        None => upstream_unavailable("unknown", "no node"),
    }
}

fn rank(
    state: &AppState,
    request_class: RequestClass,
    model: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> ollama_router_core::RankOutcome {
    let sticky = if state.config.policy.sticky_affinity {
        model.and_then(|m| state.registry.sticky_owner(m))
    } else {
        None
    };
    let tie = state
        .tie_break
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    rank_nodes(
        &state.registry.snapshot(),
        request_class,
        model,
        &state.config.policy,
        sticky.as_ref(),
        excluded,
        tie,
    )
}

#[allow(clippy::too_many_arguments)]
async fn forward_once(
    state: &AppState,
    method: &Method,
    path: &str,
    query: Option<&str>,
    incoming_headers: &HeaderMap,
    body: &Bytes,
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
) -> Result<Response, ForwardError> {
    let Some(base) = node.url.as_deref() else {
        return Err(ForwardError::Fatal {
            kind: "MissingUrl".into(),
            message: "node has no routing URL".into(),
        });
    };
    let mut upstream_path = path.to_string();
    if upstream_path.trim_end_matches('/') == "/api/embeddings" {
        upstream_path = "/api/embed".into();
    }
    let mut url = format!("{base}{upstream_path}");
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }

    let client_forward = is_client_forward(path);
    let (vram_id, ram_id) = if client_forward {
        maybe_reserve(
            &state.registry,
            &state.config.policy,
            node,
            model,
            request_class,
        )
    } else {
        (None, None)
    };
    if client_forward {
        state.registry.inflight_inc(&node.id);
    }
    let guard = InflightGuard {
        registry: Arc::clone(&state.registry),
        node_id: node.id.clone(),
        vram_id,
        ram_id,
        counts_inflight: client_forward,
    };

    let timeout = class_timeout(&state.config.timeouts, request_class);
    let headers = forward_headers(incoming_headers);
    let permit = state
        .pool
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ForwardError::Fatal {
            kind: "PoolClosed".into(),
            message: "connection pool closed".into(),
        })?;

    let mut builder = state.client.request(method.clone(), &url);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if !body.is_empty() {
        builder = builder.body(body.clone());
    }
    let request = builder.timeout(timeout).build().map_err(|err| {
        let err = err.without_url();
        ForwardError::Fatal {
            kind: "RequestBuild".into(),
            message: err.to_string(),
        }
    })?;

    let response = match state.client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            drop(guard);
            drop(permit);
            return Err(classify_reqwest(err));
        }
    };

    let status = response.status();
    if state
        .config
        .policy
        .retry_on_status
        .contains(&status.as_u16())
    {
        drop(response);
        drop(guard);
        drop(permit);
        return Err(ForwardError::Overload {
            status: status.as_u16(),
        });
    }

    let mut out_headers = response_headers(response.headers());
    if state.config.debug_headers {
        if let Ok(value) = HeaderValue::from_str(node.id.as_str()) {
            out_headers.insert(HeaderName::from_static("x-ollama-router-upstream"), value);
        }
        if let Ok(value) = HeaderValue::from_str(request_class.as_str()) {
            out_headers.insert(HeaderName::from_static("x-ollama-router-class"), value);
        }
    }

    state.registry.mark_request_success(&node.id);
    tracing::debug!(
        path,
        request_class = %request_class,
        node = %node.id,
        status = status.as_u16(),
        "route"
    );

    let token = CancellationToken::new();
    let cancel_guard = token.clone().drop_guard();
    let stream = ProxyStream {
        inner: Box::pin(response.bytes_stream()),
        collector: IncrementalCollector::new(),
        inflight: Some(guard),
        _permit: Some(permit),
        _cancel: cancel_guard,
        node: node.id.as_str().to_string(),
        model: model.map(str::to_string),
        class: request_class,
    };

    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = status;
    *res.headers_mut() = out_headers;
    Ok(res)
}

fn is_client_forward(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/'),
        "/api/generate" | "/api/chat" | "/api/embed" | "/api/embeddings"
    )
}

fn maybe_reserve(
    registry: &Registry,
    policy: &PolicyConfig,
    node: &NodeSnapshot,
    model: Option<&str>,
    request_class: RequestClass,
) -> (Option<u64>, Option<u64>) {
    let Some(model) = model else {
        return (None, None);
    };
    if node.has_model_loaded(model) {
        return (None, None);
    }
    let vram = estimate_request_vram_gb(request_class, Some(model), policy);
    let ram = estimate_request_ram_gb(node, request_class, Some(model), policy);
    (
        registry.reserve_vram(&node.id, model, vram),
        registry.reserve_ram(&node.id, model, ram),
    )
}

fn class_timeout(timeouts: &TimeoutsConfig, request_class: RequestClass) -> Duration {
    let secs = match request_class {
        RequestClass::Embed => timeouts.embed_seconds,
        RequestClass::Small | RequestClass::Medium | RequestClass::Large => {
            timeouts.generate_seconds
        }
        RequestClass::Pull => timeouts.pull_seconds,
        RequestClass::Generic => timeouts.default_seconds,
    };
    Duration::from_secs_f64(secs)
}

fn classify_reqwest(err: reqwest::Error) -> ForwardError {
    let err = err.without_url();
    let message = err.to_string();
    if err.is_timeout() && err.is_connect() {
        ForwardError::Retryable {
            reason: "connect_timeout",
            message,
        }
    } else if err.is_connect() {
        ForwardError::Retryable {
            reason: "connect_error",
            message,
        }
    } else if err.is_timeout() {
        ForwardError::Retryable {
            reason: "read_timeout",
            message,
        }
    } else if err.is_request() {
        ForwardError::Retryable {
            reason: "protocol_error",
            message,
        }
    } else {
        ForwardError::Fatal {
            kind: "reqwest".into(),
            message,
        }
    }
}

struct InflightGuard {
    registry: Arc<Registry>,
    node_id: NodeId,
    vram_id: Option<u64>,
    ram_id: Option<u64>,
    counts_inflight: bool,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.counts_inflight {
            self.registry.inflight_dec(&self.node_id);
        }
        self.registry
            .release_vram(&self.node_id, self.vram_id.take());
        self.registry.release_ram(&self.node_id, self.ram_id.take());
    }
}

struct ProxyStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    collector: IncrementalCollector,
    inflight: Option<InflightGuard>,
    _permit: Option<OwnedSemaphorePermit>,
    _cancel: tokio_util::sync::DropGuard,
    node: String,
    model: Option<String>,
    class: RequestClass,
}

impl Stream for ProxyStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                this.collector.feed(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(err))) => {
                let err = err.without_url();
                Poll::Ready(Some(Err(std::io::Error::other(truncate(
                    &err.to_string(),
                    200,
                )))))
            }
            Poll::Ready(None) => {
                this.collector.flush();
                let timing = &this.collector.timing;
                tracing::debug!(
                    node = %this.node,
                    model = this.model.as_deref().unwrap_or(""),
                    request_class = %this.class,
                    wall_seconds = timing.wall_seconds(),
                    eval_tokens = timing.eval_tokens,
                    "upstream_timing"
                );
                this.inflight.take();
                this._permit.take();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

enum ForwardError {
    Overload {
        status: u16,
    },
    Retryable {
        reason: &'static str,
        message: String,
    },
    Fatal {
        kind: String,
        message: String,
    },
}

enum BodyCapError {
    InvalidContentLength,
    TooLarge,
}

async fn read_body_capped(
    headers: &HeaderMap,
    body: Body,
    max_bytes: u64,
) -> Result<Bytes, BodyCapError> {
    if let Some(value) = headers.get(header::CONTENT_LENGTH) {
        let raw = value
            .to_str()
            .map_err(|_| BodyCapError::InvalidContentLength)?;
        let declared: i64 = raw
            .parse()
            .map_err(|_| BodyCapError::InvalidContentLength)?;
        if declared < 0 {
            return Err(BodyCapError::InvalidContentLength);
        }
        if declared as u64 > max_bytes {
            return Err(BodyCapError::TooLarge);
        }
    }
    let limit = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let limited = Limited::new(body, limit);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(_) => Err(BodyCapError::TooLarge),
    }
}

fn extract_model(body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let data: Value = serde_json::from_slice(body).ok()?;
    data.get("model")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn forward_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in incoming {
        let lowered = name.as_str();
        if HOP_BY_HOP.contains(&lowered)
            || lowered == "host"
            || lowered == "content-length"
            || lowered == "accept-encoding"
        {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }
    headers
}

fn response_headers(incoming: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in incoming {
        let lowered = name.as_str();
        if HOP_BY_HOP.contains(&lowered) || lowered == "content-length" {
            continue;
        }
        headers.append(name.clone(), value.clone());
    }
    headers
}

fn aggregated_tags(state: &AppState) -> Response {
    let models: Vec<Value> = state
        .registry
        .aggregated_tags()
        .into_iter()
        .map(|(name, nodes)| {
            json!({
                "name": name,
                "model": name,
                "details": { "router_nodes": nodes },
            })
        })
        .collect();
    let mut res = json_error(StatusCode::OK, json!({ "models": models }), None);
    if state.config.debug_headers {
        res.headers_mut().insert(
            HeaderName::from_static("x-ollama-router-aggregated"),
            HeaderValue::from_static("true"),
        );
    }
    res
}

async fn fleet_pull(state: &AppState, model: Option<&str>) -> Response {
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({"error": "model is required"}),
            None,
        );
    };
    match state.orchestrator.ensure(model).await {
        Ok(job) if job.status == JobStatus::Success => {
            json_error(StatusCode::OK, json!({"status": "success"}), None)
        }
        Ok(job) => json_error(
            StatusCode::BAD_GATEWAY,
            json!({
                "error": format!(
                    "ollama-router: pull partial failure for model {model}; see admin /router/v1/jobs/{}",
                    job.id
                ),
                "job_id": job.id,
            }),
            None,
        ),
        Err(OrchestratorError::NoPlacementTargets) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": format!(
                    "ollama-router: no placement-eligible target nodes for the requested models (model: {model}) [reason: {}]",
                    RoutingError::Capacity.as_reason_code()
                )
            }),
            Some(30),
        ),
        Err(OrchestratorError::NotConfigured) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "ollama-router: job orchestrator is not configured"}),
            Some(30),
        ),
        Err(other) => json_error(
            StatusCode::BAD_GATEWAY,
            json!({"error": format!("ollama-router: {other}")}),
            None,
        ),
    }
}

async fn fleet_delete(state: &AppState, model: Option<&str>) -> Response {
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            json!({"error": "model is required"}),
            None,
        );
    };
    match state.orchestrator.delete(model).await {
        Ok(job) if job.status == JobStatus::Success => {
            json_error(StatusCode::OK, json!({"status": "success"}), None)
        }
        Ok(job) => json_error(
            StatusCode::BAD_GATEWAY,
            json!({
                "error": format!("ollama-router: delete partial failure for model {model}"),
                "job_id": job.id,
            }),
            None,
        ),
        Err(OrchestratorError::NoTargetNodes) => {
            json_error(StatusCode::OK, json!({"status": "success"}), None)
        }
        Err(OrchestratorError::NotConfigured) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "ollama-router: job orchestrator is not configured"}),
            Some(30),
        ),
        Err(other) => json_error(
            StatusCode::BAD_GATEWAY,
            json!({"error": format!("ollama-router: {other}")}),
            None,
        ),
    }
}

async fn auto_pull_miss(state: &AppState, model: &str, request_class: RequestClass) -> Response {
    let retry_after = state.config.policy.pull_miss_retry_after_seconds;
    match state.orchestrator.ensure(model).await {
        Ok(job) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": format!(
                    "ollama-router: model {model} missing; pull enqueued on placement nodes * (job {}, retry in {retry_after}s)",
                    job.id
                ),
                "reason": "pull_enqueued",
                "job_id": job.id,
                "model": model,
                "retry_after_seconds": retry_after,
            }),
            Some(retry_after),
        ),
        Err(_) => no_candidate_response(RoutingError::ModelMissing, Some(model), request_class),
    }
}

fn no_candidate_response(
    reason: RoutingError,
    model: Option<&str>,
    request_class: RequestClass,
) -> Response {
    let mut detail = format!("ollama-router: {}", reason.message());
    if let Some(model) = model {
        detail.push_str(&format!(" (model: {model})"));
    }
    detail.push_str(&format!(
        " [class: {request_class}, reason: {}]",
        reason.as_reason_code()
    ));
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": detail}),
        reason.retry_after_seconds(),
    )
}

fn upstream_unavailable(kind: &str, message: &str) -> Response {
    json_error(
        StatusCode::BAD_GATEWAY,
        json!({
            "error": format!(
                "ollama-router: upstream unavailable ({}: {})",
                kind,
                truncate(message, 200)
            )
        }),
        None,
    )
}

fn json_error(status: StatusCode, body: Value, retry_after: Option<u32>) -> Response {
    let mut res = axum::Json(body).into_response();
    *res.status_mut() = status;
    if let Some(seconds) = retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    res
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
