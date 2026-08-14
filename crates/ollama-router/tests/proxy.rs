//! httpmock coverage for streaming, retry, body cap, tags, and 503/502.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use httpmock::prelude::*;
use ollama_router::http::{make_app, AppState};
use ollama_router_core::cloud::DemandScale;
use ollama_router_core::config::{
    Capacity, NodeConfig, PolicyConfig, RouterConfig, TimeoutsConfig,
};
use ollama_router_core::fleet::NodeId;
use ollama_router_core::routing::RoutingError;
use serde_json::{json, Value};
use tokio::time::sleep;
use tower::ServiceExt;

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("node id")
}

fn node(id: &str, url: &str, vram: f64, gpus: u32, max_inflight: Option<u32>) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: Some(url.trim_end_matches('/').to_string()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: Some(vram),
            ram_gb: Some(32.0),
            gpus: Some(gpus),
            cpu_cores: Some(8),
        },
        max_inflight,
    }
}

fn fleet_config(nodes: Vec<NodeConfig>) -> RouterConfig {
    RouterConfig {
        nodes,
        ..RouterConfig::default()
    }
}

fn state_from(config: RouterConfig) -> AppState {
    AppState::from_config(config).expect("state")
}

fn mark_ready(state: &AppState, id: &str, models: &[&str]) {
    let id = nid(id);
    state.registry.set_healthy(&id);
    state.registry.update_models(&id, models.iter().copied());
}

async fn send(state: AppState, req: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let response = make_app(state).oneshot(req).await.expect("oneshot");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, body)
}

fn json_req(method: Method, path: &str, body: Value) -> Request<Body> {
    let bytes = serde_json::to_vec(&body).expect("json");
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .expect("request")
}

#[tokio::test]
async fn generate_stream_matches_upstream_chunks() {
    let server = MockServer::start();
    let stream = b"{\"model\":\"x\",\"done\":false}\n{\"model\":\"x\",\"done\":true}\n";
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/x-ndjson")
            .body(stream);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);

    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "llama3.2:3b", "prompt": "hi", "stream": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), stream);
    mock.assert();
}

#[tokio::test]
async fn metrics_after_generate_includes_requests_total() {
    let server = MockServer::start();
    let stream = b"{\"model\":\"x\",\"done\":true}\n";
    server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/x-ndjson")
            .body(stream);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let (status, _, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "llama3.2:3b", "prompt": "hi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (ms, _, body) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ollama_router_requests_total"), "{text}");
}

#[tokio::test]
async fn embeddings_rewrites_to_embed() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);

    let (status, _, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/embeddings",
            json!({"model": "qwen3-embedding:8b", "input": ["hello"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    mock.assert();
}

#[tokio::test]
async fn retry_on_503_excludes_failed_node() {
    let a = MockServer::start();
    let c = MockServer::start();
    let fail = a.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(503).body(r#"{"error":"busy"}"#);
    });
    let ok = c.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &a.base_url(), 8.0, 1, None),
        node("node-c", &c.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-c", &["qwen3-embedding:8b"]);

    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("node-c")
    );
    fail.assert();
    ok.assert();
}

#[tokio::test]
async fn connect_failure_retries_next_node() {
    let c = MockServer::start();
    let ok = c.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![
            node("node-a", "http://127.0.0.1:1", 8.0, 1, None),
            node("node-c", &c.base_url(), 24.0, 1, None),
        ],
        timeouts: TimeoutsConfig {
            connect_seconds: 0.2,
            ..TimeoutsConfig::default()
        },
        ..RouterConfig::default()
    });
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-c", &["qwen3-embedding:8b"]);

    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("node-c")
    );
    ok.assert();
}

#[tokio::test]
async fn status_500_passes_through_without_retry() {
    let a = MockServer::start();
    let c = MockServer::start();
    let fail = a.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(500).body(r#"{"error":"boom"}"#);
    });
    let unused = c.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200).body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &a.base_url(), 8.0, 1, None),
        node("node-c", &c.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-c", &["qwen3-embedding:8b"]);

    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("node-a")
    );
    fail.assert();
    unused.assert_calls(0);
}

#[tokio::test]
async fn invalid_content_length_is_400() {
    let state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/generate")
        .header(header::CONTENT_LENGTH, "nope")
        .body(Body::from(r#"{"model":"llama3.2:3b"}"#))
        .unwrap();
    let (status, _, body) = send(state, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("invalid Content-Length"));
}

#[tokio::test]
async fn oversize_declared_body_is_413() {
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", "http://127.0.0.1:9", 8.0, 1, None)],
        policy: PolicyConfig {
            max_request_body_bytes: 8,
            ..PolicyConfig::default()
        },
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let payload = br#"{"model":"llama3.2:3b"}"#;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/generate")
        .header(header::CONTENT_LENGTH, payload.len().to_string())
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (status, _, body) = send(state, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["max_request_body_bytes"], 8);
}

#[tokio::test]
async fn aggregated_tags_union() {
    let state = state_from(fleet_config(vec![
        node("node-a", "http://127.0.0.1:9", 8.0, 1, None),
        node("node-b", "http://127.0.0.1:9", 0.0, 0, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b", "llama3.2:3b"]);
    mark_ready(&state, "node-b", &["qwen3-embedding:0.6b", "llama3.2:3b"]);
    let (status, headers, body) = send(
        state,
        Request::builder()
            .uri("/api/tags")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-aggregated")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let names: Vec<&str> = parsed["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"qwen3-embedding:8b"));
    assert!(names.contains(&"qwen3-embedding:0.6b"));
    assert!(names.contains(&"llama3.2:3b"));
}

#[tokio::test]
async fn aggregated_openai_models_union() {
    let state = state_from(fleet_config(vec![
        node("node-a", "http://127.0.0.1:9", 8.0, 1, None),
        node("node-b", "http://127.0.0.1:9", 0.0, 0, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b", "llama3.2:3b"]);
    mark_ready(&state, "node-b", &["qwen3-embedding:0.6b", "llama3.2:3b"]);
    let (status, headers, body) = send(
        state,
        Request::builder()
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-aggregated")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["object"], "list");
    let data = parsed["data"].as_array().unwrap();
    let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids.iter().filter(|id| **id == "llama3.2:3b").count(), 1);
    assert!(ids.contains(&"qwen3-embedding:8b"));
    assert!(ids.contains(&"qwen3-embedding:0.6b"));
    assert!(ids.contains(&"llama3.2:3b"));
    for item in data {
        assert_eq!(item["object"], "model");
        assert_eq!(item["created"], 0);
        assert_eq!(item["owned_by"], "library");
    }
}

#[tokio::test]
async fn aggregated_models_gauges_and_discovery_counter() {
    let state = state_from(fleet_config(vec![
        node("node-a", "http://127.0.0.1:9", 8.0, 1, None),
        node("node-b", "http://127.0.0.1:9", 0.0, 0, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b", "llama3.2:3b"]);
    mark_ready(&state, "node-b", &["qwen3-embedding:0.6b", "llama3.2:3b"]);
    let (ms, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ollama_router_aggregated_models"), "{text}");
    assert!(text.contains("ollama_router_node_models"), "{text}");
    assert!(text.contains("ollama_router_aggregated_models 3"), "{text}");
    let (status, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, body) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("ollama_router_discovery_total") && text.contains("openai_models"),
        "{text}"
    );
}

#[tokio::test]
async fn no_healthy_returns_503_retry_after() {
    let state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("30")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("no_healthy_nodes"));
}

#[tokio::test]
async fn all_saturated_returns_503_without_upstream() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200).body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &server.base_url(), 8.0, 1, Some(1)),
        node("node-c", &server.base_url(), 24.0, 1, Some(1)),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-c", &["qwen3-embedding:8b"]);
    state.registry.inflight_inc(&nid("node-a"));
    state.registry.inflight_inc(&nid("node-c"));

    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("30")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("all_nodes_saturated"));
    mock.assert_calls(0);
}

#[tokio::test]
async fn connect_failure_retries_then_502_when_exhausted() {
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", "http://127.0.0.1:1", 8.0, 1, None)],
        policy: PolicyConfig {
            retry_max_attempts: 1,
            ..PolicyConfig::default()
        },
        timeouts: TimeoutsConfig {
            connect_seconds: 0.2,
            ..TimeoutsConfig::default()
        },
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("upstream unavailable"));
}

#[tokio::test]
async fn client_disconnect_releases_inflight() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .delay(Duration::from_secs(30))
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3-embedding:0.6b"]);
    let registry = std::sync::Arc::clone(&state.registry);
    let gpu = nid("gpu");

    let app = make_app(state);
    let req = json_req(
        Method::POST,
        "/api/embed",
        json!({"model": "qwen3-embedding:0.6b", "input": ["x"]}),
    );
    let handle = tokio::spawn(app.oneshot(req));

    let started = Instant::now();
    loop {
        if registry.inflight(&gpu) == 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("inflight never incremented");
        }
        sleep(Duration::from_millis(10)).await;
    }

    handle.abort();
    let _ = handle.await;

    let started = Instant::now();
    loop {
        if registry.inflight(&gpu) == 0 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("inflight not released after client disconnect");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn readyz_503_when_nothing_healthy() {
    let state = state_from(RouterConfig::default());
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ready"], false);
}

#[tokio::test]
async fn fleet_pull_succeeds_when_upstream_has_model() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"llama3.2:3b"}]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let (status, _, body) = send(
        state,
        json_req(Method::POST, "/api/pull", json!({"model": "llama3.2:3b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "success");
}

#[tokio::test]
async fn pull_missing_model_is_400() {
    let state = state_from(RouterConfig::default());
    let (status, _, body) = send(state, json_req(Method::POST, "/api/pull", json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("model is required"));
}

fn auto_pull_policy(retry_after: u32, wait_seconds: f64) -> PolicyConfig {
    PolicyConfig {
        auto_pull_on_miss: true,
        pull_miss_retry_after_seconds: retry_after,
        auto_pull_wait_seconds: wait_seconds,
        ..PolicyConfig::default()
    }
}

fn mock_empty_tags(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
}

fn mock_pull<'a>(
    server: &'a MockServer,
    delay: Duration,
    status: u16,
    body: &str,
) -> httpmock::Mock<'a> {
    let body = body.to_string();
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(status)
            .header("content-type", "application/x-ndjson")
            .delay(delay)
            .body(body);
    })
}

struct RecordingDemand {
    called: AtomicBool,
}

impl DemandScale for RecordingDemand {
    fn request_scale_up(&self, reason: RoutingError) {
        assert_eq!(reason, RoutingError::Capacity);
        self.called.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn auto_pull_disabled_by_default() {
    let cpu = MockServer::start();
    let gpu = MockServer::start();
    mock_empty_tags(&cpu);
    mock_empty_tags(&gpu);
    let cpu_pull = mock_pull(&cpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let gpu_pull = mock_pull(&gpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let state = state_from(fleet_config(vec![
        node("cpu", &cpu.base_url(), 0.0, 0, None),
        node("gpu", &gpu.base_url(), 8.0, 1, None),
    ]));
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "brand-new:1b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.get(header::RETRY_AFTER).is_none());
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("model_missing"), "{error}");
    assert!(!error.contains("pull enqueued"), "{error}");
    cpu_pull.assert_calls(0);
    gpu_pull.assert_calls(0);
}

#[tokio::test]
async fn auto_pull_on_miss_enqueues_on_placement_nodes() {
    let cpu = MockServer::start();
    let gpu = MockServer::start();
    mock_empty_tags(&cpu);
    mock_empty_tags(&gpu);
    let cpu_pull = mock_pull(&cpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let gpu_pull = mock_pull(&gpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let mut state = state_from(RouterConfig {
        nodes: vec![
            node("cpu", &cpu.base_url(), 0.0, 0, None),
            node("gpu", &gpu.base_url(), 8.0, 1, None),
        ],
        policy: auto_pull_policy(7, 0.0),
        ..RouterConfig::default()
    });
    state.admin_token = Some("secret".into());
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, headers, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "brand-new:1b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("7")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["reason"], "pull_enqueued");
    assert_eq!(parsed["model"], "brand-new:1b");
    assert_eq!(parsed["retry_after_seconds"], 7);
    let job_id = parsed["job_id"].as_str().expect("job_id");
    let nodes = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(nodes.contains(&"cpu"), "{nodes:?}");
    assert!(nodes.contains(&"gpu"), "{nodes:?}");
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("pull enqueued"), "{error}");

    let started = Instant::now();
    loop {
        if cpu_pull.calls() >= 1 && gpu_pull.calls() >= 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!(
                "expected pulls on cpu+gpu, hits cpu={} gpu={}",
                cpu_pull.calls(),
                gpu_pull.calls()
            );
        }
        sleep(Duration::from_millis(10)).await;
    }

    let (job_status, _, job_body) = send(
        state,
        Request::builder()
            .uri(format!("/router/v1/jobs/{job_id}"))
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(job_status, StatusCode::OK);
    let job: Value = serde_json::from_slice(&job_body).unwrap();
    assert_eq!(job["id"], job_id);
}

#[tokio::test]
async fn auto_pull_miss_on_ineligible_fleet_returns_capacity_503() {
    let cpu = MockServer::start();
    let gpu = MockServer::start();
    mock_empty_tags(&cpu);
    mock_empty_tags(&gpu);
    let cpu_pull = mock_pull(&cpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let gpu_pull = mock_pull(&gpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let demand = Arc::new(RecordingDemand {
        called: AtomicBool::new(false),
    });
    let mut state = state_from(RouterConfig {
        nodes: vec![
            node("cpu", &cpu.base_url(), 0.0, 0, None),
            node("gpu", &gpu.base_url(), 8.0, 1, None),
        ],
        policy: auto_pull_policy(10, 0.0),
        ..RouterConfig::default()
    });
    state.demand = demand.clone();
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/chat",
            json!({"model": "llama3.1:405b", "messages": [{"role": "user", "content": "hi"}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("30")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("insufficient_capacity"), "{error}");
    cpu_pull.assert_calls(0);
    gpu_pull.assert_calls(0);
    assert!(demand.called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn auto_pull_miss_respects_placement() {
    let cpu = MockServer::start();
    let a = MockServer::start();
    let c = MockServer::start();
    mock_empty_tags(&cpu);
    mock_empty_tags(&a);
    mock_empty_tags(&c);
    let cpu_pull = mock_pull(&cpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let a_pull = mock_pull(&a, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let c_pull = mock_pull(&c, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let state = state_from(RouterConfig {
        nodes: vec![
            node("cpu", &cpu.base_url(), 0.0, 0, None),
            node("node-a", &a.base_url(), 8.0, 1, None),
            node("node-c", &c.base_url(), 24.0, 1, None),
        ],
        policy: auto_pull_policy(10, 0.0),
        ..RouterConfig::default()
    });
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    mark_ready(&state, "node-a", &["llama3.2:3b"]);
    mark_ready(&state, "node-c", &["llama3.2:3b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "qwen2.5:7b", "prompt": "hi"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["reason"], "pull_enqueued");
    let nodes = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(nodes.contains(&"node-a"), "{nodes:?}");
    assert!(nodes.contains(&"node-c"), "{nodes:?}");
    assert!(!nodes.contains(&"cpu"), "{nodes:?}");

    let started = Instant::now();
    loop {
        if a_pull.calls() >= 1 && c_pull.calls() >= 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!(
                "expected GPU pulls, hits a={} c={} cpu={}",
                a_pull.calls(),
                c_pull.calls(),
                cpu_pull.calls()
            );
        }
        sleep(Duration::from_millis(10)).await;
    }
    cpu_pull.assert_calls(0);
}

#[tokio::test]
async fn auto_pull_stampede_dedupes() {
    let cpu = MockServer::start();
    let gpu = MockServer::start();
    mock_empty_tags(&cpu);
    mock_empty_tags(&gpu);
    let cpu_pull = mock_pull(
        &cpu,
        Duration::from_millis(300),
        200,
        "{\"status\":\"success\"}\n",
    );
    let gpu_pull = mock_pull(
        &gpu,
        Duration::from_millis(300),
        200,
        "{\"status\":\"success\"}\n",
    );
    let state = state_from(RouterConfig {
        nodes: vec![
            node("cpu", &cpu.base_url(), 0.0, 0, None),
            node("gpu", &gpu.base_url(), 8.0, 1, None),
        ],
        policy: auto_pull_policy(10, 0.0),
        ..RouterConfig::default()
    });
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let req = || {
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "brand-new:1b", "input": ["x"]}),
        )
    };
    let (a, b, c) = tokio::join!(
        send(state.clone(), req()),
        send(state.clone(), req()),
        send(state.clone(), req()),
    );
    let mut job_ids = Vec::new();
    for (status, _, body) in [a, b, c] {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["reason"], "pull_enqueued");
        job_ids.push(parsed["job_id"].as_str().unwrap().to_string());
    }
    assert_eq!(job_ids[0], job_ids[1]);
    assert_eq!(job_ids[0], job_ids[2]);

    let started = Instant::now();
    loop {
        if cpu_pull.calls() >= 1 && gpu_pull.calls() >= 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("stampede pulls missing");
        }
        sleep(Duration::from_millis(10)).await;
    }
    cpu_pull.assert_calls(1);
    gpu_pull.assert_calls(1);
}

#[tokio::test]
async fn auto_pull_wait_forwards_once_model_appears() {
    let gpu = MockServer::start();
    mock_empty_tags(&gpu);
    let _pull = mock_pull(
        &gpu,
        Duration::from_millis(200),
        200,
        "{\"status\":\"success\"}\n",
    );
    gpu.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", &gpu.base_url(), 8.0, 1, None)],
        policy: auto_pull_policy(10, 5.0),
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, _, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "brand-new:1b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed.get("embeddings").is_some());
    let (ms, _, text) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&text);
    assert!(
        text.contains("ollama_router_auto_pull_wait_total") && text.contains("forwarded"),
        "{text}"
    );
}

#[tokio::test]
async fn auto_pull_wait_expires_to_503() {
    let gpu = MockServer::start();
    mock_empty_tags(&gpu);
    let _pull = mock_pull(
        &gpu,
        Duration::from_secs(2),
        200,
        "{\"status\":\"success\"}\n",
    );
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", &gpu.base_url(), 8.0, 1, None)],
        policy: auto_pull_policy(4, 0.3),
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "ghost:1b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("4")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["reason"], "pull_enqueued");
}

#[tokio::test]
async fn auto_pull_wait_pull_failure_still_enqueued_503() {
    let gpu = MockServer::start();
    mock_empty_tags(&gpu);
    let _pull = mock_pull(&gpu, Duration::ZERO, 500, "{\"error\":\"no\"}\n");
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", &gpu.base_url(), 8.0, 1, None)],
        policy: auto_pull_policy(4, 2.0),
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, headers, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "ghost:1b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("4")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["reason"], "pull_enqueued");
    let (ms, _, text) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&text);
    assert!(
        text.contains("pull_finished") || text.contains("timeout"),
        "{text}"
    );
}

#[tokio::test]
async fn auto_pull_wait_disconnect_does_not_inc_inflight() {
    let gpu = MockServer::start();
    mock_empty_tags(&gpu);
    let pull = mock_pull(
        &gpu,
        Duration::from_secs(2),
        200,
        "{\"status\":\"success\"}\n",
    );
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", &gpu.base_url(), 8.0, 1, None)],
        policy: auto_pull_policy(10, 5.0),
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let registry = Arc::clone(&state.registry);
    let gpu_id = nid("gpu");
    let app = make_app(state.clone());
    let req = json_req(
        Method::POST,
        "/api/embed",
        json!({"model": "brand-new:1b", "input": ["x"]}),
    );
    let handle = tokio::spawn(app.oneshot(req));
    let started = Instant::now();
    loop {
        if !state.orchestrator.list_jobs().is_empty() {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("auto-pull job never registered");
        }
        sleep(Duration::from_millis(10)).await;
    }
    sleep(Duration::from_millis(50)).await;
    assert_eq!(registry.inflight(&gpu_id), 0);
    handle.abort();
    let _ = handle.await;
    assert_eq!(registry.inflight(&gpu_id), 0);

    let started = Instant::now();
    loop {
        if pull.calls() >= 1 {
            break;
        }
        if started.elapsed() > Duration::from_secs(3) {
            panic!("background pull did not continue after disconnect");
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(registry.inflight(&gpu_id), 0);

    let (ms, _, text) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&text);
    assert!(text.contains("disconnected"), "{text}");
}

#[tokio::test]
async fn draining_verda_is_skipped_for_client_forward() {
    let keep = MockServer::start();
    let spot = MockServer::start();
    let keep_ok = keep.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let spot_hit = spot.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.2]]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "keep",
        &keep.base_url(),
        8.0,
        1,
        None,
    )]));
    state
        .registry
        .upsert_verda(node("spot", &spot.base_url(), 24.0, 1, None));
    mark_ready(&state, "keep", &["qwen3-embedding:8b"]);
    mark_ready(&state, "spot", &["qwen3-embedding:8b"]);
    assert!(state.registry.set_draining(&nid("spot"), true));

    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("keep")
    );
    assert_eq!(spot_hit.calls(), 0);
    keep_ok.assert();
}
