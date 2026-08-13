//! httpmock coverage for streaming, retry, body cap, tags, and 503/502.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use httpmock::prelude::*;
use ollama_router::http::{make_app, AppState};
use ollama_router_core::config::{
    Capacity, NodeConfig, PolicyConfig, RouterConfig, TimeoutsConfig,
};
use ollama_router_core::fleet::NodeId;
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
        ssh: None,
        provision: None,
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
