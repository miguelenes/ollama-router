//! NDJSON streaming reverse proxy; `/api/embeddings` → `/api/embed`.
//!
//! OpenAI `POST /v1/chat/completions`, `/v1/completions`, and `/v1/embeddings`
//! are passthrough client forwards (idle + reservation). Router-originated
//! errors on `/v1/*` use the OpenAI error envelope.

mod telemetry;

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

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
use ollama_router_core::fleet::{
    normalize_model, AggregatedPs, AggregatedTag, InflightAdmit, NodeId, NodeSnapshot, Registry,
};
use ollama_router_core::http_util::reqwest_error_for_log;
use ollama_router_core::jobs::{Job, JobId, JobStatus, OrchestratorError};
use ollama_router_core::routing::{
    blocked_only_by_reservations, classify_with_size_hint, estimate_request_ram_gb,
    estimate_request_vram_gb, is_inference_path, placement_eligible_node_ids, rank_nodes,
    size_hint_from_catalog, RankOutcome, RequestClass, RoutingError, TargetSpec,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, OwnedSemaphorePermit};

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

const AUTO_PULL_POLL: Duration = Duration::from_millis(250);

/// Catch-all Ollama-compatible reverse proxy.
pub async fn handle(state: &AppState, req: Request<Body>) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);
    let clean = path.trim_end_matches('/');
    let policy = &state.config.policy;
    let local_class = |model: Option<&str>| classify_with_size_hint(&path, model, policy, None);

    if clean == "/api/tags" && method == Method::GET {
        return aggregated_tags(state);
    }
    if clean == "/api/ps" && method == Method::GET {
        return aggregated_ps(state);
    }
    if clean == "/api/version" && method == Method::GET {
        return router_version(state);
    }
    if clean == "/v1/models" && method == Method::GET {
        return aggregated_openai_models(state);
    }
    if method == Method::GET {
        if let Some(id) = openai_model_id(clean) {
            return openai_model_by_id(state, id, start);
        }
    }
    // Precedence: 501 mutate/blobs (any method) → OpenAI 501 mutations →
    // known-path wrong-method 405 → unknown /api/* or /v1/* 404.
    if is_unsupported_mutate(clean) {
        return observe_local(
            state,
            clean,
            local_class(None),
            start,
            unsupported_fleet_mutate(clean),
            Some("unsupported_fleet_mutate"),
            None,
        );
    }
    if is_unsupported_openai_mutate(&method, clean) {
        return observe_local(
            state,
            clean,
            local_class(None),
            start,
            unsupported_openai_mutate(clean),
            Some("unsupported_openai_mutate"),
            None,
        );
    }
    match path_method_decision(&method, clean) {
        PathDecision::Proceed => {}
        PathDecision::MethodNotAllowed => {
            return observe_local(
                state,
                clean,
                local_class(None),
                start,
                method_not_allowed(clean),
                Some("method_not_allowed"),
                None,
            );
        }
        PathDecision::NotFound => {
            return observe_local(
                state,
                clean,
                local_class(None),
                start,
                unknown_compat_path(clean),
                Some("unknown_compat_path"),
                None,
            );
        }
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
                model = extract_named_field(&bytes, "model");
                if model.is_none() && clean == "/api/show" {
                    model = extract_named_field(&bytes, "name");
                }
                body = bytes;
            }
            Err(BodyCapError::InvalidContentLength) => {
                return observe_local(
                    state,
                    &path,
                    local_class(model.as_deref()),
                    start,
                    router_error(
                        &path,
                        StatusCode::BAD_REQUEST,
                        "ollama-router: invalid Content-Length",
                        "invalid_content_length",
                        None,
                    ),
                    Some("invalid_content_length"),
                    model.as_deref(),
                );
            }
            Err(BodyCapError::TooLarge) => {
                let response = if uses_openai_error_shape(&path) {
                    router_error(
                        &path,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "ollama-router: request body exceeds configured limit",
                        "payload_too_large",
                        None,
                    )
                } else {
                    json_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        json!({
                            "error": "ollama-router: request body exceeds configured limit",
                            "max_request_body_bytes": state.config.policy.max_request_body_bytes,
                        }),
                        None,
                    )
                };
                return observe_local(
                    state,
                    &path,
                    local_class(model.as_deref()),
                    start,
                    response,
                    Some("payload_too_large"),
                    model.as_deref(),
                );
            }
            Err(BodyCapError::Interrupted) => {
                return observe_local(
                    state,
                    &path,
                    local_class(model.as_deref()),
                    start,
                    router_error(
                        &path,
                        StatusCode::BAD_REQUEST,
                        "ollama-router: request body interrupted",
                        "body_interrupted",
                        None,
                    ),
                    Some("body_interrupted"),
                    model.as_deref(),
                );
            }
        }
    }

    if clean == "/api/pull" && method == Method::POST {
        return fleet_pull(state, model.as_deref()).await;
    }
    if clean == "/api/delete" && method == Method::DELETE {
        return fleet_delete(state, model.as_deref()).await;
    }
    // Literal stop: same fleet-unload fan-out as unload-intent generate/chat.
    // Upstream Ollama speaks generate unload (`keep_alive: 0`), not `/api/stop`.
    if clean == "/api/stop" && method == Method::POST {
        let unload_body = match model.as_deref().filter(|m| !m.is_empty()) {
            Some(m) => Bytes::from(
                serde_json::to_vec(&json!({"model": m, "keep_alive": 0})).unwrap_or_default(),
            ),
            None => Bytes::new(),
        };
        return fleet_unload(state, "/api/generate", model.as_deref(), &unload_body).await;
    }
    if method == Method::POST
        && matches!(clean, "/api/generate" | "/api/chat")
        && is_unload_intent(clean, &body)
    {
        return fleet_unload(state, clean, model.as_deref(), &body).await;
    }

    let size_hint = model
        .as_deref()
        .and_then(|m| size_hint_from_catalog(&state.registry.aggregated_tags(), m));
    let request_class =
        classify_with_size_hint(&path, model.as_deref(), &state.config.policy, size_hint);
    proxy_ranked(
        state,
        ProxyCall {
            method,
            path: &path,
            query: query.as_deref(),
            incoming_headers: &incoming_headers,
            body,
            model,
            request_class,
        },
    )
    .await
}

/// Shared generate/chat/embed hop (ranked retry + one upstream).
struct ProxyCall<'a> {
    method: Method,
    path: &'a str,
    query: Option<&'a str>,
    incoming_headers: &'a HeaderMap,
    body: Bytes,
    model: Option<String>,
    request_class: RequestClass,
}

async fn proxy_ranked(state: &AppState, call: ProxyCall<'_>) -> Response {
    let start = Instant::now();
    let config = Arc::clone(&state.config);
    let policy = &config.policy;
    let mut excluded: HashSet<NodeId> = HashSet::new();
    let request_class = call.request_class;
    let path = call.path;
    let model = call.model.as_deref();

    let mut decision = rank_decision(state, request_class, model, &excluded);
    if !decision.outcome.ok()
        && decision.outcome.reason == Some(RoutingError::Saturated)
        && policy.saturation_wait_seconds > 0.0
    {
        let deadline =
            Instant::now() + Duration::from_secs_f64(policy.saturation_wait_seconds.max(0.0));
        tracing::info!(
            path,
            request_class = %request_class,
            wait_seconds = policy.saturation_wait_seconds,
            "saturation_wait"
        );
        loop {
            if decision.outcome.ok() || decision.outcome.reason != Some(RoutingError::Saturated) {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // Arm before re-rank so a release between check and wait is not lost.
            let notified = state.registry.slot_notified();
            decision = rank_decision(state, request_class, model, &excluded);
            if decision.outcome.ok() || decision.outcome.reason != Some(RoutingError::Saturated) {
                break;
            }
            match tokio::time::timeout(remaining, notified).await {
                Ok(()) => {
                    decision = rank_decision(state, request_class, model, &excluded);
                }
                Err(_) => break,
            }
        }
    } else if !decision.outcome.ok()
        && decision.outcome.reason == Some(RoutingError::Saturated)
        && policy.overload_wait_ms > 0
    {
        tracing::info!(
            path,
            request_class = %request_class,
            wait_ms = policy.overload_wait_ms,
            "overload_wait"
        );
        tokio::time::sleep(Duration::from_millis(u64::from(policy.overload_wait_ms))).await;
        decision = rank_decision(state, request_class, model, &excluded);
    }
    if !decision.outcome.ok()
        && decision.outcome.reason == Some(RoutingError::Capacity)
        && policy.admission_wait_ms > 0
        && blocked_only_by_reservations(&decision.nodes, request_class, model, policy)
    {
        tracing::info!(
            path,
            request_class = %request_class,
            model = model.unwrap_or(""),
            wait_ms = policy.admission_wait_ms,
            "admission_wait"
        );
        tokio::time::sleep(Duration::from_millis(u64::from(policy.admission_wait_ms))).await;
        decision = rank_decision(state, request_class, model, &excluded);
    }
    let mut outcome = decision.outcome;

    if !outcome.ok() {
        let reason = outcome.reason.unwrap_or(RoutingError::NoHealthy);
        if reason == RoutingError::ModelMissing
            && policy.auto_pull_on_miss
            && is_inference_path(path)
        {
            if let Some(model_name) = model {
                match auto_pull_on_miss(state, path, request_class, model_name, start).await {
                    AutoPullResult::Forward(next) => {
                        outcome = next;
                    }
                    AutoPullResult::Done(response) => return response,
                }
            }
        }
    }
    if !outcome.ok() {
        let reason = outcome.reason.unwrap_or(RoutingError::NoHealthy);
        if reason.requests_demand_scale_up() {
            DemandScale::request_scale_up(state.demand.as_ref(), reason);
        }
        let response = no_candidate_response(path, reason, model, request_class, policy);
        return observe_local(
            state,
            path,
            request_class,
            start,
            response,
            Some(reason.as_reason_code()),
            model,
        );
    }

    let mut ranked = outcome.ranked;
    let max_attempts = (policy.retry_max_attempts as usize)
        .min(ranked.len())
        .max(1);
    let mut last_error: Option<ForwardError> = None;
    let mut last_node_id: Option<NodeId> = None;
    for attempt in 0..max_attempts {
        if ranked.is_empty() {
            break;
        }
        let node = ranked.swap_remove(0);
        last_node_id = Some(node.id.clone());
        excluded.insert(node.id.clone());
        let has_next = attempt + 1 < max_attempts;
        match forward_once(state, &call, &node).await {
            Ok(response) => {
                state.metrics.observe_request(
                    request_class.as_str(),
                    response.status().as_u16(),
                    node.id.as_str(),
                    start.elapsed(),
                );
                return response;
            }
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
                        let response = upstream_unavailable(path, kind, message);
                        state.metrics.observe_request(
                            request_class.as_str(),
                            response.status().as_u16(),
                            node.id.as_str(),
                            start.elapsed(),
                        );
                        return response;
                    }
                }
                last_error = Some(err);
                if has_next {
                    let reranked = rank(state, request_class, model, &excluded);
                    if !reranked.ok() {
                        break;
                    }
                    ranked = reranked.ranked;
                }
            }
        }
    }

    let node = last_node_id.as_ref().map_or("-", NodeId::as_str);
    let response = match last_error {
        Some(ForwardError::Overload { status }) => {
            DemandScale::request_scale_up(state.demand.as_ref(), RoutingError::Saturated);
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                client_error_body(
                    path,
                    StatusCode::SERVICE_UNAVAILABLE,
                    &format!(
                        "ollama-router: all attempted nodes are overloaded (upstream http {status}) [class: {request_class}, reason: {}]",
                        RoutingError::Saturated.as_reason_code()
                    ),
                    RoutingError::Saturated.as_reason_code(),
                ),
                Some(policy.saturated_retry_after_seconds),
            )
        }
        Some(ForwardError::Retryable { reason, message }) => {
            upstream_unavailable(path, reason, &message)
        }
        Some(ForwardError::Fatal { kind, message }) => upstream_unavailable(path, &kind, &message),
        None => upstream_unavailable(path, "unknown", "no node"),
    };
    state.metrics.observe_request(
        request_class.as_str(),
        response.status().as_u16(),
        node,
        start.elapsed(),
    );
    response
}

struct RankDecision {
    nodes: Vec<NodeSnapshot>,
    outcome: RankOutcome,
}

fn rank(
    state: &AppState,
    request_class: RequestClass,
    model: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> RankOutcome {
    rank_decision(state, request_class, model, excluded).outcome
}

fn rank_decision(
    state: &AppState,
    request_class: RequestClass,
    model: Option<&str>,
    excluded: &HashSet<NodeId>,
) -> RankDecision {
    let nodes = state.registry.snapshot();
    let sticky = if state.config.policy.sticky_affinity {
        model.and_then(|m| Registry::sticky_owner_from(&nodes, m))
    } else {
        None
    };
    let tie = state
        .tie_break
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let outcome = rank_nodes(
        &nodes,
        request_class,
        model,
        &state.config.policy,
        sticky.as_ref(),
        excluded,
        tie,
    );
    RankDecision { nodes, outcome }
}

async fn forward_once(
    state: &AppState,
    call: &ProxyCall<'_>,
    node: &NodeSnapshot,
) -> Result<Response, ForwardError> {
    let Some(base) = node.url.as_deref() else {
        return Err(ForwardError::Fatal {
            kind: "MissingUrl".into(),
            message: "node has no routing URL".into(),
        });
    };
    let mut upstream_path = call.path.to_string();
    if upstream_path.trim_end_matches('/') == "/api/embeddings" {
        upstream_path = "/api/embed".into();
    }
    let mut url = format!("{base}{upstream_path}");
    if let Some(q) = call.query {
        url.push('?');
        url.push_str(q);
    }

    let client_forward = is_client_forward(&call.method, call.path);
    let timeout = class_timeout(&state.config.timeouts, call.request_class);
    let headers = forward_headers(call.incoming_headers);
    let permit = state
        .pool
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ForwardError::Fatal {
            kind: "PoolClosed".into(),
            message: "connection pool closed".into(),
        })?;

    let (vram_id, ram_id) = if client_forward {
        maybe_reserve(
            &state.registry,
            &state.config.policy,
            node,
            call.model.as_deref(),
            call.request_class,
        )
    } else {
        (None, None)
    };
    if client_forward {
        match state.registry.inflight_inc(&node.id) {
            InflightAdmit::Admitted => {}
            InflightAdmit::Saturated => {
                state.registry.release_vram(&node.id, vram_id);
                state.registry.release_ram(&node.id, ram_id);
                return Err(ForwardError::Retryable {
                    reason: "saturated",
                    message: "node is at inflight cap".into(),
                });
            }
            InflightAdmit::Missing | InflightAdmit::Draining => {
                state.registry.release_vram(&node.id, vram_id);
                state.registry.release_ram(&node.id, ram_id);
                return Err(ForwardError::Retryable {
                    reason: "draining",
                    message: "node is draining".into(),
                });
            }
        }
    }
    let guard = InflightGuard {
        registry: Arc::clone(&state.registry),
        node_id: node.id.clone(),
        vram_id,
        ram_id,
        counts_inflight: client_forward,
    };

    let mut builder = state.client.request(call.method.clone(), &url);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    if !call.body.is_empty() {
        builder = builder.body(call.body.clone());
    }
    let request = builder
        .timeout(timeout)
        .build()
        .map_err(|err| ForwardError::Fatal {
            kind: "RequestBuild".into(),
            message: reqwest_error_for_log(err),
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
        if let Ok(value) = HeaderValue::from_str(call.request_class.as_str()) {
            out_headers.insert(HeaderName::from_static("x-ollama-router-class"), value);
        }
    }

    tracing::debug!(
        path = call.path,
        request_class = %call.request_class,
        model = call.model.as_deref().unwrap_or(""),
        node = %node.id,
        status = status.as_u16(),
        "route"
    );

    let collector = IncrementalCollector::for_content_type(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
    );
    let stream = ProxyStream {
        inner: Box::pin(response.bytes_stream()),
        collector,
        inflight: Some(guard),
        _permit: Some(permit),
        node: node.id.as_str().to_string(),
        model: call.model.clone(),
        class: call.request_class,
    };

    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = status;
    *res.headers_mut() = out_headers;
    Ok(res)
}

fn is_client_forward(method: &Method, path: &str) -> bool {
    *method == Method::POST && is_inference_path(path)
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
    let is_timeout = err.is_timeout();
    let is_connect = err.is_connect();
    let is_request = err.is_request();
    let message = reqwest_error_for_log(err);
    if is_timeout && is_connect {
        ForwardError::Retryable {
            reason: "connect_timeout",
            message,
        }
    } else if is_connect {
        ForwardError::Retryable {
            reason: "connect_error",
            message,
        }
    } else if is_timeout {
        ForwardError::Retryable {
            reason: "read_timeout",
            message,
        }
    } else if is_request {
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
                if let Some(guard) = this.inflight.as_ref() {
                    guard.registry.mark_request_failure(&guard.node_id);
                }
                this.inflight.take();
                this._permit.take();
                let message = reqwest_error_for_log(err);
                tracing::debug!(
                    node = %this.node,
                    error = %truncate(&message, 200),
                    "upstream_stream_error"
                );
                Poll::Ready(Some(Err(std::io::Error::other(truncate(&message, 200)))))
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
                if let Some(guard) = this.inflight.as_ref() {
                    guard.registry.mark_request_success(&guard.node_id);
                }
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
    Interrupted,
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
        Err(err) => {
            if <dyn std::error::Error>::is::<http_body_util::LengthLimitError>(err.as_ref()) {
                Err(BodyCapError::TooLarge)
            } else {
                Err(BodyCapError::Interrupted)
            }
        }
    }
}

fn extract_named_field(body: &[u8], key: &str) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let data: Value = serde_json::from_slice(body).ok()?;
    data.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `ollama stop`: `keep_alive <= 0` and empty/absent prompt (generate) or messages (chat).
fn is_unload_intent(path: &str, body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let Ok(data) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(keep_alive) = data.get("keep_alive") else {
        return false;
    };
    let Some(seconds) = parse_keep_alive_seconds(keep_alive) else {
        return false;
    };
    if seconds > 0.0 {
        return false;
    }
    match path.trim_end_matches('/') {
        "/api/generate" => prompt_absent_or_empty(data.get("prompt")),
        "/api/chat" => messages_absent_or_empty(data.get("messages")),
        _ => false,
    }
}

fn prompt_absent_or_empty(prompt: Option<&Value>) -> bool {
    match prompt {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

fn messages_absent_or_empty(messages: Option<&Value>) -> bool {
    match messages {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(_) => false,
    }
}

/// Parse Ollama `keep_alive` (number of seconds, or Go-style duration string).
/// Returns `None` when unparseable (caller falls through to normal inference).
fn parse_keep_alive_seconds(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(raw) => parse_go_duration_seconds(raw.trim()),
        _ => None,
    }
}

fn parse_go_duration_seconds(raw: &str) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    if let Ok(n) = raw.parse::<f64>() {
        return Some(n);
    }
    // Go duration: optional signed decimal + unit (ns|us|µs|ms|s|m|h), possibly concatenated.
    let mut total = 0.0_f64;
    let mut rest = raw;
    let mut parsed_any = false;
    while !rest.is_empty() {
        let (sign, after_sign) = match rest.as_bytes()[0] {
            b'+' => (1.0, &rest[1..]),
            b'-' => (-1.0, &rest[1..]),
            _ => (1.0, rest),
        };
        if after_sign.is_empty() {
            return None;
        }
        let num_end = after_sign
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(after_sign.len());
        if num_end == 0 {
            return None;
        }
        let number: f64 = after_sign[..num_end].parse().ok()?;
        let after_num = &after_sign[num_end..];
        let (unit, after_unit) = after_num
            .strip_prefix("ms")
            .map(|rest| (1e-3, rest))
            .or_else(|| {
                after_num
                    .strip_prefix("us")
                    .or_else(|| after_num.strip_prefix("µs"))
                    .or_else(|| after_num.strip_prefix("μs"))
                    .map(|rest| (1e-6, rest))
            })
            .or_else(|| after_num.strip_prefix("ns").map(|rest| (1e-9, rest)))
            .or_else(|| after_num.strip_prefix('s').map(|rest| (1.0, rest)))
            .or_else(|| after_num.strip_prefix('m').map(|rest| (60.0, rest)))
            .or_else(|| after_num.strip_prefix('h').map(|rest| (3600.0, rest)))?;
        total += sign * number * unit;
        parsed_any = true;
        rest = after_unit;
    }
    parsed_any.then_some(total)
}

/// Fan-out unload to every healthy loaded holder (cordoned included; inventory draining excluded).
async fn fleet_unload(state: &AppState, path: &str, model: Option<&str>, body: &Bytes) -> Response {
    let start = Instant::now();
    let request_class = RequestClass::Generic;
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return observe_local(
            state,
            path,
            request_class,
            start,
            json_error(
                StatusCode::BAD_REQUEST,
                json!({"error": "model is required"}),
                None,
            ),
            Some("model_required"),
            None,
        );
    };
    let targets: Vec<NodeSnapshot> = state
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| n.healthy && !n.draining && n.url.is_some() && n.has_model_loaded(model))
        .collect();

    if targets.is_empty() {
        return observe_local(
            state,
            path,
            request_class,
            start,
            unload_success(model),
            None,
            Some(model),
        );
    }

    let timeout = class_timeout(&state.config.timeouts, RequestClass::Generic);
    let mut join_set = tokio::task::JoinSet::new();
    for node in targets {
        let Some(base) = node.url.as_deref().map(str::to_string) else {
            continue;
        };
        let client = state.client.clone();
        let body = body.clone();
        let path = path.to_string();
        let node_id = node.id.clone();
        join_set.spawn(async move {
            let url = format!("{base}{path}");
            let result = client
                .post(&url)
                .timeout(timeout)
                .body(body)
                .header(header::CONTENT_TYPE, "application/json")
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => Ok(node_id),
                Ok(resp) => {
                    // Drain body without logging contents.
                    let _ = resp.bytes().await;
                    Err(node_id)
                }
                Err(_) => Err(node_id),
            }
        });
    }

    let mut any_failed = false;
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(Ok(_)) => {}
            Ok(Err(node_id)) => {
                any_failed = true;
                tracing::warn!(node = %node_id, "fleet_unload_target_failed");
            }
            Err(_) => {
                any_failed = true;
                tracing::warn!("fleet_unload_join_failed");
            }
        }
    }

    if any_failed {
        return observe_local(
            state,
            path,
            request_class,
            start,
            router_error(
                path,
                StatusCode::BAD_GATEWAY,
                "ollama-router: fleet unload failed on one or more nodes",
                "unload_failed",
                None,
            ),
            Some("unload_failed"),
            Some(model),
        );
    }
    observe_local(
        state,
        path,
        request_class,
        start,
        unload_success(model),
        None,
        Some(model),
    )
}

fn unload_success(model: &str) -> Response {
    json_error(
        StatusCode::OK,
        json!({
            "model": model,
            "created_at": unload_created_at(),
            "response": "",
            "done": true,
            "done_reason": "unload",
        }),
        None,
    )
}

fn unload_created_at() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let nanos = dur.subsec_nanos();
    let (year, month, day, hour, min, sec) = civil_utc_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{nanos:09}Z")
}

/// Civil UTC Y-M-D h:m:s from Unix seconds (Howard Hinnant algorithm).
fn civil_utc_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let hour = tod / 3600;
    let min = (tod % 3600) / 60;
    let sec = tod % 60;
    // days since 1970-01-01 → civil
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d, hour, min, sec)
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
        .map(ollama_tag_json)
        .collect();
    let mut res = json_error(StatusCode::OK, json!({ "models": models }), None);
    if state.config.debug_headers {
        res.headers_mut().insert(
            HeaderName::from_static("x-ollama-router-aggregated"),
            HeaderValue::from_static("true"),
        );
    }
    state.metrics.observe_discovery("tags");
    res
}

fn aggregated_ps(state: &AppState) -> Response {
    let models: Vec<Value> = state
        .registry
        .aggregated_ps()
        .into_iter()
        .map(ollama_ps_json)
        .collect();
    let mut res = json_error(StatusCode::OK, json!({ "models": models }), None);
    if state.config.debug_headers {
        res.headers_mut().insert(
            HeaderName::from_static("x-ollama-router-aggregated"),
            HeaderValue::from_static("true"),
        );
    }
    state.metrics.observe_discovery("ps");
    res
}

fn router_version(state: &AppState) -> Response {
    let res = json_error(
        StatusCode::OK,
        json!({ "version": env!("CARGO_PKG_VERSION") }),
        None,
    );
    state.metrics.observe_discovery("version");
    res
}

fn ollama_ps_json(row: AggregatedPs) -> Value {
    let mut details = match row.details {
        Some(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    details["router_node"] = json!(row.node);
    let mut obj = json!({
        "name": row.name,
        "model": row.name,
        "digest": row.digest,
        "details": details,
    });
    if let Some(size) = row.size {
        obj["size"] = json!(size);
    }
    if let Some(size_vram) = row.size_vram {
        obj["size_vram"] = json!(size_vram);
    }
    if let Some(expires_at) = row.expires_at {
        obj["expires_at"] = json!(expires_at);
    }
    if let Some(context_length) = row.context_length {
        obj["context_length"] = json!(context_length);
    }
    obj
}

fn ollama_tag_json(row: AggregatedTag) -> Value {
    let mut details = match row.details {
        Some(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    details["router_nodes"] = json!(row.nodes);
    let mut obj = json!({
        "name": row.name,
        "model": row.name,
        "digest": row.digest,
        "details": details,
    });
    if let Some(size) = row.size {
        obj["size"] = json!(size);
    }
    if let Some(modified_at) = row.modified_at {
        obj["modified_at"] = json!(modified_at);
    }
    if let Some(capabilities) = row.capabilities {
        obj["capabilities"] = json!(capabilities);
    }
    obj
}

fn openai_model_json(row: &AggregatedTag) -> Value {
    json!({
        "id": row.name,
        "object": "model",
        "created": row.created_unix(),
        "owned_by": "library",
    })
}

fn aggregated_openai_models(state: &AppState) -> Response {
    let data: Vec<Value> = state
        .registry
        .aggregated_tags()
        .iter()
        .map(openai_model_json)
        .collect();
    let mut res = json_error(
        StatusCode::OK,
        json!({ "object": "list", "data": data }),
        None,
    );
    if state.config.debug_headers {
        res.headers_mut().insert(
            HeaderName::from_static("x-ollama-router-aggregated"),
            HeaderValue::from_static("true"),
        );
    }
    state.metrics.observe_discovery("openai_models");
    res
}

async fn fleet_pull(state: &AppState, model: Option<&str>) -> Response {
    let start = Instant::now();
    let request_class = RequestClass::Pull;
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return observe_local(
            state,
            "/api/pull",
            request_class,
            start,
            json_error(
                StatusCode::BAD_REQUEST,
                json!({"error": "model is required"}),
                None,
            ),
            Some("model_required"),
            None,
        );
    };
    let job = match state
        .orchestrator
        .start_ensure(&[model.to_string()], TargetSpec::Placement, false, false)
        .await
    {
        Ok(job) => job,
        Err(OrchestratorError::NoPlacementTargets) => {
            return observe_local(
                state,
                "/api/pull",
                request_class,
                start,
                json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({
                        "error": format!(
                            "ollama-router: no placement-eligible target nodes for the requested models (model: {model}) [reason: {}]",
                            RoutingError::Capacity.as_reason_code()
                        )
                    }),
                    Some(state.config.policy.provision_retry_after_seconds),
                ),
                Some(RoutingError::Capacity.as_reason_code()),
                Some(model),
            );
        }
        Err(OrchestratorError::NotConfigured) => {
            return observe_local(
                state,
                "/api/pull",
                request_class,
                start,
                json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": "ollama-router: job orchestrator is not configured"}),
                    Some(state.config.policy.provision_retry_after_seconds),
                ),
                Some(RoutingError::NoNodes.as_reason_code()),
                Some(model),
            );
        }
        Err(other) => {
            return observe_local(
                state,
                "/api/pull",
                request_class,
                start,
                json_error(
                    StatusCode::BAD_GATEWAY,
                    json!({"error": format!("ollama-router: {other}")}),
                    None,
                ),
                Some("orchestrator_error"),
                Some(model),
            );
        }
    };

    let watch_rx = state.orchestrator.subscribe_job(&job.id);
    let snapshot = state
        .orchestrator
        .get_job(&job.id)
        .unwrap_or_else(|| job.clone());
    let stream = job_ndjson_stream(watch_rx, snapshot, model.to_string(), "pulling", "pull");
    observe_local(
        state,
        "/api/pull",
        request_class,
        start,
        ndjson_stream_response(stream),
        None,
        Some(model),
    )
}

fn job_ndjson_stream(
    watch_rx: Option<tokio::sync::watch::Receiver<Job>>,
    initial: Job,
    model: String,
    progress_status: &'static str,
    kind_label: &'static str,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut watch_rx = watch_rx;
        let mut last_completed: Option<usize> = None;
        loop {
            let job = match watch_rx.as_mut() {
                Some(rx) => rx.borrow_and_update().clone(),
                None => initial.clone(),
            };
            let completed = job
                .targets
                .values()
                .filter(|t| !t.status.is_incomplete())
                .count();
            let terminal = !job.status.is_incomplete();
            if last_completed != Some(completed) || terminal {
                let progress = job_progress_line(&job, progress_status);
                if send_ndjson_line(&tx, &progress).await.is_err() {
                    return;
                }
                last_completed = Some(completed);
            }
            if terminal {
                let terminal_line = match job.status {
                    JobStatus::Success => json!({"status": "success"}),
                    _ => json!({
                        "error": format!(
                            "ollama-router: {kind_label} partial failure for model {model} [reason: partial_failure]"
                        )
                    }),
                };
                let _ = send_ndjson_line(&tx, &terminal_line).await;
                return;
            }
            let Some(rx) = watch_rx.as_mut() else {
                return;
            };
            if rx.changed().await.is_err() {
                let job = rx.borrow().clone();
                if !job.status.is_incomplete() {
                    let terminal_line = match job.status {
                        JobStatus::Success => json!({"status": "success"}),
                        _ => json!({
                            "error": format!(
                                "ollama-router: {kind_label} partial failure for model {model} [reason: partial_failure]"
                            )
                        }),
                    };
                    let _ = send_ndjson_line(&tx, &terminal_line).await;
                }
                return;
            }
        }
    });
    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

fn immediate_success_ndjson_stream() -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    futures_util::stream::once(async { Ok(Bytes::from("{\"status\":\"success\"}\n")) })
}

fn ndjson_stream_response(
    stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
) -> Response {
    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    res
}

async fn send_ndjson_line(
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    value: &Value,
) -> Result<(), ()> {
    let line = format!(
        "{}\n",
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    );
    tx.send(Ok(Bytes::from(line))).await.map_err(|_| ())
}

fn job_progress_line(job: &Job, status: &str) -> Value {
    let total = job.targets.len();
    let completed = job
        .targets
        .values()
        .filter(|t| !t.status.is_incomplete())
        .count();
    json!({
        "status": status,
        "total": total,
        "completed": completed,
    })
}

async fn fleet_delete(state: &AppState, model: Option<&str>) -> Response {
    let start = Instant::now();
    let request_class = RequestClass::Generic;
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return observe_local(
            state,
            "/api/delete",
            request_class,
            start,
            json_error(
                StatusCode::BAD_REQUEST,
                json!({"error": "model is required"}),
                None,
            ),
            Some("model_required"),
            None,
        );
    };
    let job = match state
        .orchestrator
        .start_delete(&[model.to_string()], TargetSpec::Placement, false)
        .await
    {
        Ok(job) => job,
        Err(OrchestratorError::NoTargetNodes) => {
            return observe_local(
                state,
                "/api/delete",
                request_class,
                start,
                ndjson_stream_response(immediate_success_ndjson_stream()),
                None,
                Some(model),
            );
        }
        Err(OrchestratorError::NotConfigured) => {
            return observe_local(
                state,
                "/api/delete",
                request_class,
                start,
                json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": "ollama-router: job orchestrator is not configured"}),
                    Some(state.config.policy.provision_retry_after_seconds),
                ),
                Some(RoutingError::NoNodes.as_reason_code()),
                Some(model),
            );
        }
        Err(other) => {
            return observe_local(
                state,
                "/api/delete",
                request_class,
                start,
                json_error(
                    StatusCode::BAD_GATEWAY,
                    json!({"error": format!("ollama-router: {other}")}),
                    None,
                ),
                Some("orchestrator_error"),
                Some(model),
            );
        }
    };

    let watch_rx = state.orchestrator.subscribe_job(&job.id);
    let snapshot = state
        .orchestrator
        .get_job(&job.id)
        .unwrap_or_else(|| job.clone());
    let stream = job_ndjson_stream(watch_rx, snapshot, model.to_string(), "deleting", "delete");
    observe_local(
        state,
        "/api/delete",
        request_class,
        start,
        ndjson_stream_response(stream),
        None,
        Some(model),
    )
}

enum AutoPullResult {
    Forward(RankOutcome),
    Done(Response),
}

struct WaitMetricGuard {
    metrics: Arc<crate::http::Metrics>,
    outcome: Option<&'static str>,
}

impl WaitMetricGuard {
    fn record(&mut self, outcome: &'static str) {
        self.outcome = Some(outcome);
    }
}

impl Drop for WaitMetricGuard {
    fn drop(&mut self) {
        self.metrics
            .observe_auto_pull_wait(self.outcome.unwrap_or("disconnected"));
    }
}

fn observe_local(
    state: &AppState,
    path: &str,
    request_class: RequestClass,
    start: Instant,
    response: Response,
    reason: Option<&str>,
    model: Option<&str>,
) -> Response {
    let status = response.status().as_u16();
    state
        .metrics
        .observe_request(request_class.as_str(), status, "-", start.elapsed());
    if let Some(reason) = reason {
        state.metrics.route_reason(reason);
        tracing::warn!(
            path,
            request_class = %request_class,
            model = model.unwrap_or(""),
            status,
            reason,
            "route_rejected"
        );
    }
    response
}

fn observe_reject(
    state: &AppState,
    path: &str,
    request_class: RequestClass,
    start: Instant,
    response: Response,
    reason: &str,
    model: Option<&str>,
) -> Response {
    observe_local(
        state,
        path,
        request_class,
        start,
        response,
        Some(reason),
        model,
    )
}

fn pull_enqueued_response(
    path: &str,
    model: &str,
    job_id: &JobId,
    nodes: &[NodeId],
    retry_after: u32,
) -> Response {
    let node_ids: Vec<&str> = nodes.iter().map(NodeId::as_str).collect();
    let nodes_fmt = node_ids.join(", ");
    let message = format!(
        "ollama-router: model {model} missing; pull enqueued on placement nodes {nodes_fmt} (job {job_id}, retry in {retry_after}s)"
    );
    let mut body = client_error_body(
        path,
        StatusCode::SERVICE_UNAVAILABLE,
        &message,
        "pull_enqueued",
    );
    if let Some(obj) = body.as_object_mut() {
        obj.insert("reason".into(), json!("pull_enqueued"));
        obj.insert("job_id".into(), json!(job_id.to_string()));
        obj.insert("model".into(), json!(model));
        obj.insert("nodes".into(), json!(node_ids));
        obj.insert("retry_after_seconds".into(), json!(retry_after));
    }
    json_error(StatusCode::SERVICE_UNAVAILABLE, body, Some(retry_after))
}

async fn auto_pull_on_miss(
    state: &AppState,
    path: &str,
    request_class: RequestClass,
    model: &str,
    start: Instant,
) -> AutoPullResult {
    let policy = &state.config.policy;
    let size_hint = size_hint_from_catalog(&state.registry.aggregated_tags(), model);
    let eligible = placement_eligible_node_ids(
        &state.registry.snapshot(),
        model,
        policy,
        false,
        false,
        size_hint,
    );
    if eligible.is_empty() {
        tracing::warn!(
            model,
            request_class = %request_class,
            reason = RoutingError::Capacity.as_reason_code(),
            "auto_pull_no_capacity"
        );
        DemandScale::request_scale_up(state.demand.as_ref(), RoutingError::Capacity);
        let response = no_candidate_response(
            path,
            RoutingError::Capacity,
            Some(model),
            request_class,
            policy,
        );
        return AutoPullResult::Done(observe_reject(
            state,
            path,
            request_class,
            start,
            response,
            RoutingError::Capacity.as_reason_code(),
            Some(model),
        ));
    }

    let job = match state
        .orchestrator
        .start_ensure(&[model.to_string()], TargetSpec::Placement, false, false)
        .await
    {
        Ok(job) => job,
        Err(_) => {
            let response = no_candidate_response(
                path,
                RoutingError::ModelMissing,
                Some(model),
                request_class,
                policy,
            );
            return AutoPullResult::Done(observe_reject(
                state,
                path,
                request_class,
                start,
                response,
                RoutingError::ModelMissing.as_reason_code(),
                Some(model),
            ));
        }
    };

    let node_ids: Vec<&str> = eligible.iter().map(NodeId::as_str).collect();
    tracing::info!(
        event = "pull_enqueued",
        model,
        request_class = %request_class,
        job_id = %job.id,
        nodes = ?node_ids,
        "pull_enqueued"
    );
    let retry_after = policy.pull_miss_retry_after_seconds;
    let wait = policy.auto_pull_wait_seconds;
    if wait <= 0.0 {
        let response = pull_enqueued_response(path, model, &job.id, &eligible, retry_after);
        return AutoPullResult::Done(observe_reject(
            state,
            path,
            request_class,
            start,
            response,
            "pull_enqueued",
            Some(model),
        ));
    }

    wait_for_pull(
        state,
        PullWait {
            path,
            request_class,
            model,
            start,
            job_id: job.id,
            eligible: &eligible,
            retry_after,
            wait_seconds: wait,
        },
    )
    .await
}

struct PullWait<'a> {
    path: &'a str,
    request_class: RequestClass,
    model: &'a str,
    start: Instant,
    job_id: JobId,
    eligible: &'a [NodeId],
    retry_after: u32,
    wait_seconds: f64,
}

async fn wait_for_pull(state: &AppState, wait: PullWait<'_>) -> AutoPullResult {
    let PullWait {
        path,
        request_class,
        model,
        start,
        job_id,
        eligible,
        retry_after,
        wait_seconds,
    } = wait;
    let deadline = Instant::now() + Duration::from_secs_f64(wait_seconds);
    let mut guard = WaitMetricGuard {
        metrics: Arc::clone(&state.metrics),
        outcome: None,
    };
    loop {
        if Instant::now() >= deadline {
            guard.record("timeout");
            let response = pull_enqueued_response(path, model, &job_id, eligible, retry_after);
            return AutoPullResult::Done(observe_reject(
                state,
                path,
                request_class,
                start,
                response,
                "pull_enqueued",
                Some(model),
            ));
        }

        let snap = state.registry.snapshot();
        let size_hint = size_hint_from_catalog(&state.registry.aggregated_tags(), model);
        let holders = placement_eligible_node_ids(
            &snap,
            model,
            &state.config.policy,
            false,
            false,
            size_hint,
        );
        let present = holders.iter().any(|id| {
            snap.iter()
                .any(|node| node.id == *id && node.healthy && node.has_model(model))
        });
        if present {
            let ranked = rank(state, request_class, Some(model), &HashSet::new());
            if ranked.ok() {
                guard.record("forwarded");
                return AutoPullResult::Forward(ranked);
            }
            guard.record("pull_finished");
            let response = pull_enqueued_response(path, model, &job_id, eligible, retry_after);
            return AutoPullResult::Done(observe_reject(
                state,
                path,
                request_class,
                start,
                response,
                "pull_enqueued",
                Some(model),
            ));
        }

        if state
            .orchestrator
            .get_job(&job_id)
            .is_some_and(|job| !job.status.is_incomplete())
        {
            guard.record("pull_finished");
            let response = pull_enqueued_response(path, model, &job_id, eligible, retry_after);
            return AutoPullResult::Done(observe_reject(
                state,
                path,
                request_class,
                start,
                response,
                "pull_enqueued",
                Some(model),
            ));
        }

        tokio::time::sleep(AUTO_PULL_POLL).await;
    }
}

fn no_candidate_response(
    path: &str,
    reason: RoutingError,
    model: Option<&str>,
    request_class: RequestClass,
    policy: &PolicyConfig,
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
        client_error_body(
            path,
            StatusCode::SERVICE_UNAVAILABLE,
            &detail,
            reason.as_reason_code(),
        ),
        reason.retry_after_seconds(policy),
    )
}

fn upstream_unavailable(path: &str, kind: &str, message: &str) -> Response {
    let detail = format!(
        "ollama-router: upstream unavailable ({}: {})",
        kind,
        truncate(message, 200)
    );
    json_error(
        StatusCode::BAD_GATEWAY,
        client_error_body(
            path,
            StatusCode::BAD_GATEWAY,
            &detail,
            "upstream_unavailable",
        ),
        None,
    )
}

fn uses_openai_error_shape(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p == "/v1" || p.starts_with("/v1/")
}

/// Known-path × allowed-method table for `/api/*` and `/v1/*`.
/// Mutate/blobs and OpenAI mutation 501s are handled before this is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathDecision {
    Proceed,
    MethodNotAllowed,
    NotFound,
}

fn path_method_decision(method: &Method, path: &str) -> PathDecision {
    let p = path.trim_end_matches('/');
    let under_api = p == "/api" || p.starts_with("/api/");
    let under_v1 = p == "/v1" || p.starts_with("/v1/");
    if !under_api && !under_v1 {
        return PathDecision::Proceed;
    }
    match known_compat_methods(p) {
        Some(allowed) if allowed.iter().any(|m| m == method) => PathDecision::Proceed,
        Some(_) => PathDecision::MethodNotAllowed,
        None => PathDecision::NotFound,
    }
}

fn known_compat_methods(path: &str) -> Option<&'static [Method]> {
    if openai_model_id(path).is_some() {
        return Some(&[Method::GET]);
    }
    match path {
        "/api/tags" | "/api/ps" | "/api/version" => Some(&[Method::GET]),
        "/api/generate" | "/api/chat" | "/api/embed" | "/api/embeddings" | "/api/show"
        | "/api/pull" | "/api/stop" | "/api/push" | "/api/copy" | "/api/create" => {
            Some(&[Method::POST])
        }
        "/api/delete" => Some(&[Method::DELETE]),
        "/v1/models" => Some(&[Method::GET]),
        "/v1/chat/completions" | "/v1/completions" | "/v1/embeddings" => Some(&[Method::POST]),
        _ => None,
    }
}

fn openai_model_id(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/models/").filter(|id| !id.is_empty())
}

fn is_unsupported_mutate(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    matches!(p, "/api/push" | "/api/copy" | "/api/create") || p.starts_with("/api/blobs")
}

fn is_unsupported_openai_mutate(method: &Method, path: &str) -> bool {
    let p = path.trim_end_matches('/');
    if *method == Method::DELETE && openai_model_id(p).is_some() {
        return true;
    }
    if *method == Method::POST && (p == "/v1/fine_tuning" || p.starts_with("/v1/fine_tuning/")) {
        return true;
    }
    false
}

fn openai_error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 404 | 405 | 413 => "invalid_request_error",
        _ => "server_error",
    }
}

fn client_error_body(path: &str, status: StatusCode, message: &str, code: &str) -> Value {
    if uses_openai_error_shape(path) {
        json!({
            "error": {
                "message": message,
                "type": openai_error_type(status),
                "code": code,
            }
        })
    } else {
        json!({"error": message})
    }
}

fn router_error(
    path: &str,
    status: StatusCode,
    message: &str,
    code: &str,
    retry_after: Option<u32>,
) -> Response {
    json_error(
        status,
        client_error_body(path, status, message, code),
        retry_after,
    )
}

fn method_not_allowed(path: &str) -> Response {
    router_error(
        path,
        StatusCode::METHOD_NOT_ALLOWED,
        "ollama-router: method not allowed",
        "method_not_allowed",
        None,
    )
}

fn unknown_compat_path(path: &str) -> Response {
    if uses_openai_error_shape(path) {
        router_error(
            path,
            StatusCode::NOT_FOUND,
            "ollama-router: unknown OpenAI-compatible path",
            "unknown_path",
            None,
        )
    } else {
        router_error(
            path,
            StatusCode::NOT_FOUND,
            "ollama-router: unknown path",
            "unknown_path",
            None,
        )
    }
}

fn unsupported_fleet_mutate(path: &str) -> Response {
    json_error(
        StatusCode::NOT_IMPLEMENTED,
        json!({
            "error": format!(
                "ollama-router: {path} is not a fleet operation; use POST /api/pull or admin /router/v1/models/ensure [reason: not_a_fleet_operation]"
            )
        }),
        None,
    )
}

fn unsupported_openai_mutate(path: &str) -> Response {
    router_error(
        path,
        StatusCode::NOT_IMPLEMENTED,
        "ollama-router: operation is not a fleet operation",
        "not_a_fleet_operation",
        None,
    )
}

/// Aggregated retrieve for `GET /v1/models/{id}`. Soft-fail/partial like the list.
pub(crate) fn openai_model_by_id(state: &AppState, id: &str, start: Instant) -> Response {
    let decoded = percent_decode_path(id);
    let target = normalize_model(&decoded);
    let request_class = RequestClass::Generic;
    let found = state
        .registry
        .aggregated_tags()
        .into_iter()
        .find(|row| row.name == target);
    match found {
        Some(row) => {
            state.metrics.observe_discovery("openai_models");
            observe_local(
                state,
                "/v1/models",
                request_class,
                start,
                json_error(StatusCode::OK, openai_model_json(&row), None),
                None,
                Some(&decoded),
            )
        }
        None => observe_local(
            state,
            "/v1/models",
            request_class,
            start,
            router_error(
                "/v1/models",
                StatusCode::NOT_FOUND,
                &format!("The model '{decoded}' does not exist"),
                "model_not_found",
                None,
            ),
            Some("model_not_found"),
            Some(&decoded),
        ),
    }
}

fn percent_decode_path(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((high << 4) | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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

#[cfg(test)]
mod observability_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use ollama_router_core::config::RouterConfig;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Registry;

    struct Capture(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for Capture
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut msg = String::new();
            let mut visitor = FieldVisitor(&mut msg);
            event.record(&mut visitor);
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", event.metadata().name(), msg));
        }
    }

    struct FieldVisitor<'a>(&'a mut String);

    impl tracing::field::Visit for FieldVisitor<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}=\"{}\"", field.name(), value));
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            self.0.push_str(&format!("{}={:?}", field.name(), value));
        }
    }

    #[test]
    fn route_rejected_log_includes_model_and_reason() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Registry::default().with(Capture(logs.clone()));
        tracing::subscriber::with_default(subscriber, || {
            observe_local(
                &crate::http::AppState::from_config(RouterConfig::default()).expect("state"),
                "/api/generate",
                RequestClass::Small,
                Instant::now(),
                json_error(StatusCode::SERVICE_UNAVAILABLE, json!({"error": "x"}), None),
                Some("model_missing"),
                Some("llama3.2:3b"),
            );
        });
        let joined = logs.lock().unwrap().join(" ");
        assert!(joined.contains("route_rejected"), "{joined}");
        assert!(joined.contains("model=\"llama3.2:3b\""), "{joined}");
        assert!(joined.contains("reason=\"model_missing\""), "{joined}");
    }
}

#[cfg(test)]
mod unload_tests {
    use super::*;

    #[test]
    fn keep_alive_number_and_duration() {
        assert_eq!(parse_keep_alive_seconds(&json!(0)), Some(0.0));
        assert_eq!(parse_keep_alive_seconds(&json!(-1)), Some(-1.0));
        assert_eq!(parse_keep_alive_seconds(&json!(5)), Some(5.0));
        assert_eq!(parse_keep_alive_seconds(&json!("0")), Some(0.0));
        assert_eq!(parse_keep_alive_seconds(&json!("0s")), Some(0.0));
        assert_eq!(parse_keep_alive_seconds(&json!("-1m")), Some(-60.0));
        assert_eq!(parse_keep_alive_seconds(&json!("1h30m")), Some(5400.0));
        assert!(parse_keep_alive_seconds(&json!("nope")).is_none());
        assert!(parse_keep_alive_seconds(&json!(true)).is_none());
    }

    #[test]
    fn unload_intent_generate_and_chat() {
        assert!(is_unload_intent(
            "/api/generate",
            br#"{"model":"m","keep_alive":0}"#
        ));
        assert!(is_unload_intent(
            "/api/generate",
            br#"{"model":"m","keep_alive":"0s","prompt":""}"#
        ));
        assert!(!is_unload_intent(
            "/api/generate",
            br#"{"model":"m","keep_alive":0,"prompt":"hi"}"#
        ));
        assert!(is_unload_intent(
            "/api/chat",
            br#"{"model":"m","keep_alive":0}"#
        ));
        assert!(is_unload_intent(
            "/api/chat",
            br#"{"model":"m","keep_alive":0,"messages":[]}"#
        ));
        assert!(!is_unload_intent(
            "/api/chat",
            br#"{"model":"m","keep_alive":0,"messages":[{"role":"user","content":"x"}]}"#
        ));
        assert!(!is_unload_intent(
            "/api/generate",
            br#"{"model":"m","keep_alive":5}"#
        ));
        assert!(!is_unload_intent(
            "/api/generate",
            br#"{"model":"m","keep_alive":"bogus"}"#
        ));
    }

    #[test]
    fn path_method_table_405_and_404() {
        assert_eq!(
            path_method_decision(&Method::POST, "/api/tags"),
            PathDecision::MethodNotAllowed
        );
        assert_eq!(
            path_method_decision(&Method::GET, "/v1/chat/completions"),
            PathDecision::MethodNotAllowed
        );
        assert_eq!(
            path_method_decision(&Method::GET, "/api/not-a-real-endpoint"),
            PathDecision::NotFound
        );
        assert_eq!(
            path_method_decision(&Method::POST, "/api/generate"),
            PathDecision::Proceed
        );
        assert_eq!(
            path_method_decision(&Method::POST, "/api/stop"),
            PathDecision::Proceed
        );
    }

    #[test]
    fn openai_mutate_paths_are_detected() {
        assert!(is_unsupported_openai_mutate(
            &Method::DELETE,
            "/v1/models/qwen3:8b"
        ));
        assert!(is_unsupported_openai_mutate(
            &Method::POST,
            "/v1/fine_tuning/jobs"
        ));
        assert!(!is_unsupported_openai_mutate(
            &Method::GET,
            "/v1/models/qwen3:8b"
        ));
        assert!(!is_unsupported_openai_mutate(
            &Method::POST,
            "/v1/chat/completions"
        ));
    }
}
