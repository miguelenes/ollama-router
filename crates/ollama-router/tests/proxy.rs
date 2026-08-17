//! httpmock coverage for streaming, retry, body cap, tags, and 503/502.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use bytes::Bytes;
use futures_util::stream;
use http_body_util::BodyExt;
use httpmock::prelude::*;
use ollama_router::http::{make_app, AppState};
use ollama_router_core::cloud::{
    CachedOffer, CloudProviderHandle, DemandScale, MultiProviderDemand,
};
use ollama_router_core::config::{
    Capacity, NodeConfig, PolicyConfig, RouterConfig, TimeoutsConfig, UpstreamPoolConfig,
};
use ollama_router_core::fleet::{NodeId, PsRecord, TagRecord};
use ollama_router_core::routing::RoutingError;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

fn mark_ready_with_record(state: &AppState, id: &str, name: &str, record: TagRecord) {
    let id = nid(id);
    state.registry.set_healthy(&id);
    state
        .registry
        .update_models_from_records(&id, [(name, record)]);
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
async fn interrupted_body_is_400() {
    let state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let chunks = stream::iter([
        Ok::<_, std::io::Error>(Bytes::from_static(br#"{"model":"llama3.2:3b"}"#)),
        Err(std::io::Error::other("client gone")),
    ]);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/generate")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(chunks))
        .unwrap();
    let (status, _, body) = send(state, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let err = parsed["error"].as_str().unwrap();
    assert!(err.contains("interrupted"), "{err}");
    assert!(!err.contains("exceeds configured limit"), "{err}");
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
    for model in parsed["models"].as_array().unwrap() {
        let digest = model["digest"].as_str().unwrap();
        assert!(digest.len() >= 12, "{digest}");
        assert!(model["details"]["router_nodes"].is_array());
    }
}

#[tokio::test]
async fn aggregated_tags_includes_probe_cli_fields() {
    let state = state_from(fleet_config(vec![node(
        "local",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    mark_ready_with_record(
        &state,
        "local",
        "llama3.2:1b",
        TagRecord {
            digest: "55fc3abd386771e5b5d1bbcc732f3c3f4df6e9f9f08f1131f9cc27ba2d1eec5b".into(),
            size: Some(1321098329),
            modified_at: Some("2026-08-01T00:00:00Z".into()),
            details: Some(json!({"family": "llama"})),
            capabilities: Some(vec!["completion".into()]),
        },
    );
    let (status, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/api/tags")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let model = &parsed["models"][0];
    assert_eq!(model["name"], "llama3.2:1b");
    assert_eq!(
        model["digest"],
        "55fc3abd386771e5b5d1bbcc732f3c3f4df6e9f9f08f1131f9cc27ba2d1eec5b"
    );
    assert_eq!(model["size"], 1321098329);
    assert_eq!(model["modified_at"], "2026-08-01T00:00:00Z");
    assert_eq!(model["details"]["family"], "llama");
    assert_eq!(model["details"]["router_nodes"], json!(["local"]));
    assert_eq!(model["capabilities"], json!(["completion"]));
    assert!(state
        .registry
        .last_client_request_at(&nid("local"))
        .is_none());
    assert_eq!(state.registry.inflight(&nid("local")), 0);
}

#[tokio::test]
async fn aggregated_tags_digest_conflict_keeps_newest() {
    let state = state_from(fleet_config(vec![
        node("a", "http://127.0.0.1:9", 8.0, 1, None),
        node("b", "http://127.0.0.1:10", 24.0, 1, None),
    ]));
    mark_ready_with_record(
        &state,
        "a",
        "llama3.2:1b",
        TagRecord {
            digest: "aaaaaaaaaaaa".into(),
            size: Some(1),
            modified_at: Some("2026-01-01T00:00:00Z".into()),
            details: None,
            capabilities: None,
        },
    );
    mark_ready_with_record(
        &state,
        "b",
        "llama3.2:1b",
        TagRecord {
            digest: "bbbbbbbbbbbb".into(),
            size: Some(2),
            modified_at: Some("2026-08-01T00:00:00Z".into()),
            details: None,
            capabilities: None,
        },
    );
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/api/tags")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let models = parsed["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["digest"], "bbbbbbbbbbbb");
    assert_eq!(models[0]["details"]["router_nodes"], json!(["a", "b"]));
}

#[tokio::test]
async fn aggregated_tags_omits_unhealthy_holders() {
    let state = state_from(fleet_config(vec![
        node("a", "http://127.0.0.1:9", 8.0, 1, None),
        node("b", "http://127.0.0.1:10", 24.0, 1, None),
    ]));
    mark_ready_with_record(
        &state,
        "a",
        "llama3.2:1b",
        TagRecord {
            digest: "aaaaaaaaaaaa".into(),
            size: Some(1),
            modified_at: Some("2026-01-01T00:00:00Z".into()),
            details: None,
            capabilities: None,
        },
    );
    mark_ready_with_record(
        &state,
        "b",
        "llama3.2:1b",
        TagRecord {
            digest: "bbbbbbbbbbbb".into(),
            size: Some(2),
            modified_at: Some("2026-08-01T00:00:00Z".into()),
            details: None,
            capabilities: None,
        },
    );
    state.registry.set_unhealthy(&nid("b"));
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/api/tags")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let models = parsed["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["digest"], "aaaaaaaaaaaa");
    assert_eq!(models[0]["details"]["router_nodes"], json!(["a"]));
}

#[tokio::test]
async fn openai_models_created_from_modified_at() {
    let state = state_from(fleet_config(vec![node(
        "local",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    mark_ready_with_record(
        &state,
        "local",
        "llama3.2:1b",
        TagRecord {
            digest: "aaaaaaaaaaaa".into(),
            size: Some(1),
            modified_at: Some("2026-08-01T00:00:00Z".into()),
            details: None,
            capabilities: None,
        },
    );
    let (status, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let created = parsed["data"][0]["created"].as_i64().unwrap();
    assert!(created > 0, "{created}");
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/v1/models/llama3.2:1b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["created"], created);
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
async fn saturation_wait_admits_when_slot_frees() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let mut config = fleet_config(vec![node("gpu", &server.base_url(), 8.0, 1, Some(1))]);
    config.policy.saturation_wait_seconds = 2.0;
    let state = state_from(config);
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);
    assert_eq!(
        state.registry.inflight_inc(&nid("gpu")),
        ollama_router_core::fleet::InflightAdmit::Admitted
    );

    let wait_state = state.clone();
    let waiting = tokio::spawn(async move {
        send(
            wait_state,
            json_req(
                Method::POST,
                "/api/embed",
                json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
            ),
        )
        .await
    });

    sleep(Duration::from_millis(50)).await;
    state.registry.inflight_dec(&nid("gpu"));

    let (status, _, body) = waiting.await.expect("join");
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed.get("embeddings").is_some(), "{parsed}");
    mock.assert();
}

#[tokio::test]
async fn saturation_wait_expiry_is_503_with_retry_after() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200).body(r#"{"embeddings":[[0.1]]}"#);
    });
    let mut config = fleet_config(vec![node("gpu", &server.base_url(), 8.0, 1, Some(1))]);
    config.policy.saturation_wait_seconds = 0.15;
    let state = state_from(config);
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);
    state.registry.inflight_inc(&nid("gpu"));

    let started = Instant::now();
    let (status, headers, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(100));
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
    let (status, headers, body) = send(
        state,
        json_req(Method::POST, "/api/pull", json!({"model": "llama3.2:3b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("ndjson"))
        .collect();
    assert!(
        lines.iter().any(|l| l["status"] == "success"),
        "expected success line in {lines:?}"
    );
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

struct FakeProviderHandle {
    name: &'static str,
    offer: Option<CachedOffer>,
    below_ceiling: bool,
    hits: AtomicU32,
}

impl CloudProviderHandle for FakeProviderHandle {
    fn provider(&self) -> &'static str {
        self.name
    }

    fn request_scale_up(&self, reason: RoutingError) {
        assert!(
            matches!(
                reason,
                RoutingError::Capacity | RoutingError::Saturated | RoutingError::NoHealthy
            ),
            "{reason:?}"
        );
        self.hits.fetch_add(1, Ordering::SeqCst);
    }

    fn cached_best_offer(&self) -> Option<CachedOffer> {
        self.offer
    }

    fn below_ceiling(&self) -> bool {
        self.below_ceiling
    }
}

#[tokio::test]
async fn capacity_miss_triggers_one_provider_create_with_retry_after() {
    let handle = Arc::new(FakeProviderHandle {
        name: "runpod",
        offer: Some(CachedOffer {
            hourly_price: 0.39,
            vram_gb: Some(24.0),
        }),
        below_ceiling: true,
        hits: AtomicU32::new(0),
    });
    let demand = Arc::new(MultiProviderDemand::new(vec![
        handle.clone() as Arc<dyn CloudProviderHandle>
    ]));
    let mut state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    state.demand = demand;
    // Model is on disk but the 8 GiB node cannot place a 405B-class request.
    mark_ready(&state, "gpu", &["llama3.1:405b"]);
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
    assert_eq!(handle.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn non_client_traffic_never_triggers_demand_scale() {
    let handle = Arc::new(FakeProviderHandle {
        name: "runpod",
        offer: Some(CachedOffer {
            hourly_price: 0.39,
            vram_gb: Some(24.0),
        }),
        below_ceiling: true,
        hits: AtomicU32::new(0),
    });
    let demand = Arc::new(MultiProviderDemand::new(vec![
        handle.clone() as Arc<dyn CloudProviderHandle>
    ]));
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/ps");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
    let mut state = state_from(RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1, None)],
        policy: PolicyConfig {
            model_warm_enabled: true,
            model_warm_interval_seconds: 0.05,
            model_warm_cooldown_seconds: 0.05,
            model_warm_min_free_vram_gb: 0.0,
            ..PolicyConfig::default()
        },
        ..RouterConfig::default()
    });
    state.demand = demand;
    state.admin_token = Some("secret".into());
    mark_ready(&state, "gpu", &["llama3.2:1b"]);

    let (hz, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(hz, StatusCode::OK);

    let (ps, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/api/ps")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ps, StatusCode::OK);

    let (admin, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/router/v1/stats")
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(admin, StatusCode::OK);

    let warm = tokio::spawn(ollama_router::warm::run(
        state.clone(),
        tokio_util::sync::CancellationToken::new(),
    ));
    sleep(Duration::from_millis(200)).await;
    warm.abort();

    assert_eq!(
        handle.hits.load(Ordering::SeqCst),
        0,
        "health /api/ps / admin / warm-keeper must not scale"
    );
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

#[tokio::test]
async fn cordoned_holder_is_never_selected() {
    let sole = MockServer::start();
    let _hit = sole.mock(|when, then| {
        when.method(POST).path("/api/chat");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"message":{"role":"assistant","content":"nope"}}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "desk",
        &sole.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "desk", &["qwen3:8b"]);
    assert!(state.registry.set_cordoned(&nid("desk"), true));

    let (status, _, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/chat",
            json!({"model": "qwen3:8b", "messages": [{"role":"user","content":"hi"}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("no_healthy_nodes") || error.contains("model_missing"),
        "{parsed}"
    );
    assert_eq!(_hit.calls(), 0);

    assert!(state.registry.set_cordoned(&nid("desk"), false));
    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/chat",
            json!({"model": "qwen3:8b", "messages": [{"role":"user","content":"hi"}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("desk")
    );
}

#[tokio::test]
async fn unload_fans_out_to_every_loaded_holder() {
    let a = MockServer::start();
    let b = MockServer::start();
    let a_unload = a.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"done":true,"done_reason":"unload"}"#);
    });
    let b_unload = b.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"done":true,"done_reason":"unload"}"#);
    });
    let state = state_from(fleet_config(vec![
        node("a", &a.base_url(), 24.0, 1, None),
        node("b", &b.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "a", &["qwen3:8b"]);
    mark_ready(&state, "b", &["qwen3:8b"]);
    state
        .registry
        .update_ps_state(&nid("a"), ["qwen3:8b"], Some(4.0));
    state
        .registry
        .update_ps_state(&nid("b"), ["qwen3:8b"], Some(4.0));

    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "qwen3:8b", "keep_alive": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["done"], true);
    assert_eq!(parsed["done_reason"], "unload");
    assert_eq!(parsed["response"], "");
    a_unload.assert();
    b_unload.assert();
}

#[tokio::test]
async fn unload_of_unloaded_model_is_success() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["other:1b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "gone:1b", "keep_alive": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["done_reason"], "unload");
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn non_empty_prompt_with_keep_alive_zero_stays_ranked() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"hi","done":true}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3:8b"]);
    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "qwen3:8b", "prompt": "hi", "keep_alive": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-upstream")
            .and_then(|v| v.to_str().ok()),
        Some("gpu")
    );
    mock.assert();
}

#[tokio::test]
async fn unload_still_hits_cordoned_loaded_holder() {
    let server = MockServer::start();
    let unload = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"done":true,"done_reason":"unload"}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "desk",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "desk", &["qwen3:8b"]);
    state
        .registry
        .update_ps_state(&nid("desk"), ["qwen3:8b"], Some(4.0));
    assert!(state.registry.set_cordoned(&nid("desk"), true));
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "qwen3:8b", "keep_alive": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["done_reason"], "unload");
    unload.assert();
}

#[tokio::test]
async fn unload_does_not_set_last_client_request_at() {
    let server = MockServer::start();
    let _unload = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body(r#"{"done":true}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3:8b"]);
    state
        .registry
        .update_ps_state(&nid("gpu"), ["qwen3:8b"], Some(4.0));
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
    let (status, _, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "qwen3:8b", "keep_alive": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
}

#[tokio::test]
async fn cancel_running_pull_ndjson_is_terminal_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_secs(30))
            .body("{\"status\":\"success\"}\n");
    });
    let config = fleet_config(vec![node("gpu", &server.base_url(), 24.0, 1, None)]);
    let mut state = state_from(config);
    state.admin_token = Some("secret".into());
    mark_ready(&state, "gpu", &[]);

    let pull_state = state.clone();
    let pull = tokio::spawn(async move {
        send(
            pull_state,
            json_req(Method::POST, "/api/pull", json!({"model": "moondream"})),
        )
        .await
    });

    let job_id = {
        let started = Instant::now();
        loop {
            let jobs = state.orchestrator.list_jobs();
            if let Some(job) = jobs.first() {
                break job.id;
            }
            if started.elapsed() > Duration::from_secs(2) {
                panic!("job never appeared");
            }
            sleep(Duration::from_millis(10)).await;
        }
    };

    let (cancel_status, _, _) = send(
        state.clone(),
        Request::builder()
            .method(Method::POST)
            .uri(format!("/router/v1/jobs/{job_id}/cancel"))
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(cancel_status, StatusCode::OK);

    let (status, headers, body) = pull.await.expect("join");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("ndjson"))
        .collect();
    assert!(
        lines.iter().any(|l| l.get("error").is_some()),
        "expected terminal error line in {lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|l| l.get("status") != Some(&json!("success"))),
        "must not emit success after cancel: {lines:?}"
    );
}

async fn spawn_broken_ndjson_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head).await;
                let _ = sock.write_all(br#"{"model":"x"}"#).await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn pool_wait_does_not_hold_inflight_or_reservations() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .delay(Duration::from_secs(30))
            .body(r#"{"response":"ok"}"#);
    });
    let mut config = fleet_config(vec![node("gpu", &server.base_url(), 24.0, 1, None)]);
    config.upstream = UpstreamPoolConfig {
        max_connections: 1,
        max_keepalive_connections: 1,
    };
    let state = state_from(config);
    mark_ready(&state, "gpu", &["llama3.1:8b"]);
    let registry = std::sync::Arc::clone(&state.registry);
    let gpu = nid("gpu");
    let app = make_app(state.clone());
    let first = tokio::spawn(app.oneshot(json_req(
        Method::POST,
        "/api/generate",
        json!({"model": "llama3.1:8b", "prompt": "x"}),
    )));
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
    let reserved = registry.reserved_vram_gb(&gpu);
    assert!(reserved > 0.0, "cold load should reserve vram");

    let app2 = make_app(state);
    let second = tokio::spawn(app2.oneshot(json_req(
        Method::POST,
        "/api/generate",
        json!({"model": "llama3.1:8b", "prompt": "y"}),
    )));
    sleep(Duration::from_millis(150)).await;
    assert_eq!(registry.inflight(&gpu), 1);
    assert!((registry.reserved_vram_gb(&gpu) - reserved).abs() < 1e-9);

    second.abort();
    let _ = second.await;
    sleep(Duration::from_millis(50)).await;
    assert_eq!(registry.inflight(&gpu), 1);

    first.abort();
    let _ = first.await;
    let started = Instant::now();
    loop {
        if registry.inflight(&gpu) == 0 {
            break;
        }
        if started.elapsed() > Duration::from_secs(2) {
            panic!("inflight not released after abort");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn mid_stream_failure_credits_fail_streak_without_retry() {
    let (broken, broken_h) = spawn_broken_ndjson_upstream().await;
    let other = MockServer::start();
    let other_hit = other.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &broken, 8.0, 1, None),
        node("node-b", &other.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-b", &["qwen3-embedding:8b"]);
    let a = nid("node-a");
    state.registry.mark_request_failure(&a);
    state.registry.mark_request_failure(&a);
    assert_eq!(state.registry.fail_streak(&a), 2);
    assert!(state.registry.get(&a).unwrap().healthy);

    let response = make_app(state.clone())
        .oneshot(json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await;
    assert_eq!(state.registry.fail_streak(&a), 3);
    assert!(!state.registry.get(&a).unwrap().healthy);
    assert_eq!(other_hit.calls(), 0);
    broken_h.abort();
}

#[tokio::test]
async fn complete_stream_clears_fail_streak() {
    let server = MockServer::start();
    let _ok = server.mock(|when, then| {
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
    let gpu = nid("gpu");
    state.registry.mark_request_failure(&gpu);
    state.registry.mark_request_failure(&gpu);
    assert_eq!(state.registry.fail_streak(&gpu), 2);
    let (status, _, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/api/embed",
            json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.registry.fail_streak(&gpu), 0);
}

#[tokio::test]
async fn loaded_vram_capacity_miss_skips_admission_wait() {
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", "http://127.0.0.1:9", 24.0, 1, None)],
        policy: PolicyConfig {
            admission_wait_ms: 500,
            ..PolicyConfig::default()
        },
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.1:8b"]);
    state
        .registry
        .update_ps_state(&nid("gpu"), ["other:1b"], Some(20.0));
    let started = Instant::now();
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "llama3.1:8b", "prompt": "x"}),
        ),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("insufficient_capacity"), "{error}");
    assert!(
        elapsed < Duration::from_millis(300),
        "admission wait should not run on loaded-vram miss: {elapsed:?}"
    );
}

#[tokio::test]
async fn large_generate_unknown_vram_returns_insufficient_capacity() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"nope"}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![NodeConfig {
            id: nid("unknown"),
            url: Some(server.base_url().trim_end_matches('/').to_string()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: None,
                ram_gb: Some(32.0),
                gpus: None,
                cpu_cores: Some(8),
            },
            max_inflight: None,
        }],
        ..RouterConfig::default()
    });
    mark_ready(&state, "unknown", &["llama3.1:70b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "llama3.1:70b", "prompt": "x"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_str().unwrap();
    assert!(error.contains("insufficient_capacity"), "{error}");
    hit.assert_calls(0);
}

#[tokio::test]
async fn small_generate_still_forwards_to_unknown_vram() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"response":"ok"}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![NodeConfig {
            id: nid("unknown"),
            url: Some(server.base_url().trim_end_matches('/').to_string()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: None,
                ram_gb: Some(32.0),
                gpus: None,
                cpu_cores: Some(8),
            },
            max_inflight: None,
        }],
        ..RouterConfig::default()
    });
    mark_ready(&state, "unknown", &["llama3.2:3b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/generate",
            json!({"model": "llama3.2:3b", "prompt": "x"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["response"], "ok");
    hit.assert_calls(1);
}

#[tokio::test]
async fn inflight_cas_does_not_overshoot_cap() {
    let a_srv = MockServer::start();
    let b_srv = MockServer::start();
    let _a = a_srv.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .delay(Duration::from_millis(400))
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.1]]}"#);
    });
    let _b = b_srv.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200)
            .delay(Duration::from_millis(400))
            .header("content-type", "application/json")
            .body(r#"{"embeddings":[[0.2]]}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &a_srv.base_url(), 8.0, 1, Some(1)),
        node("node-b", &b_srv.base_url(), 8.0, 1, Some(1)),
    ]));
    mark_ready(&state, "node-a", &["qwen3-embedding:8b"]);
    mark_ready(&state, "node-b", &["qwen3-embedding:8b"]);
    let registry = std::sync::Arc::clone(&state.registry);
    let a = nid("node-a");
    let max_a = Arc::new(AtomicU32::new(0));
    let max_watch = Arc::clone(&max_a);
    let watch = tokio::spawn(async move {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            let n = registry.inflight(&a);
            max_watch.fetch_max(n, Ordering::Relaxed);
            sleep(Duration::from_millis(5)).await;
        }
    });
    let r1 = tokio::spawn(make_app(state.clone()).oneshot(json_req(
        Method::POST,
        "/api/embed",
        json!({"model": "qwen3-embedding:8b", "input": ["x"]}),
    )));
    let r2 = tokio::spawn(make_app(state).oneshot(json_req(
        Method::POST,
        "/api/embed",
        json!({"model": "qwen3-embedding:8b", "input": ["y"]}),
    )));
    let _ = r1.await;
    let _ = r2.await;
    watch.abort();
    assert!(max_a.load(Ordering::Relaxed) <= 1);
}

fn openai_chat(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false
    })
}

#[tokio::test]
async fn openai_chat_sets_last_client_request_at() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"ok"}}]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let gpu = nid("gpu");
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    let (status, _, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.registry.last_client_request_at(&gpu).is_some());
    mock.assert();
}

#[tokio::test]
async fn openai_get_chat_and_ps_do_not_set_last_client() {
    let server = MockServer::start();
    let ps = server.mock(|when, then| {
        when.method(GET).path("/api/ps");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
    let chat = server.mock(|when, then| {
        when.method(GET).path("/v1/chat/completions");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let gpu = nid("gpu");

    let (chat_status, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/v1/chat/completions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        chat_status == StatusCode::NOT_FOUND || chat_status == StatusCode::METHOD_NOT_ALLOWED,
        "GET /v1/chat/completions must not forward, got {chat_status}"
    );
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(chat.calls(), 0);

    let (ps_status, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/api/ps")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ps_status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["models"], json!([]));
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(ps.calls(), 0, "client GET /api/ps must not hit upstream");
}

#[tokio::test]
async fn openai_chat_cold_load_reserves_vram() {
    let server = MockServer::start();
    let _mock = server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .delay(Duration::from_secs(30))
            .body(r#"{"id":"cmpl-1"}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.1:8b"]);
    let registry = std::sync::Arc::clone(&state.registry);
    let gpu = nid("gpu");
    let handle = tokio::spawn(make_app(state).oneshot(json_req(
        Method::POST,
        "/v1/chat/completions",
        openai_chat("llama3.1:8b"),
    )));
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
    assert!(
        registry.reserved_vram_gb(&gpu) > 0.0,
        "cold OpenAI chat should reserve vram"
    );
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn openai_paths_set_request_class_header() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/embeddings");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"data":[{"embedding":[0.1]}]}"#);
    });
    server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"cmpl-1"}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        80.0,
        1,
        None,
    )]));
    mark_ready(
        &state,
        "gpu",
        &["all-minilm", "llama3.1:70b", "llama3.2:3b"],
    );

    let (_, headers, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/v1/embeddings",
            json!({"model": "all-minilm", "input": ["hello"]}),
        ),
    )
    .await;
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("embed")
    );

    let (_, headers, _) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.1:70b"),
        ),
    )
    .await;
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("large")
    );

    let (_, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
        ),
    )
    .await;
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("small")
    );
}

#[tokio::test]
async fn openai_chat_sse_matches_upstream_chunks() {
    let server = MockServer::start();
    let stream = b"data: {\"id\":\"cmpl-1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n";
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "text/event-stream")
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
            "/v1/chat/completions",
            json!({
                "model": "llama3.2:3b",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), stream);
    mock.assert();
}

#[tokio::test]
async fn openai_embeddings_does_not_rewrite_to_embed() {
    let server = MockServer::start();
    let openai = server.mock(|when, then| {
        when.method(POST).path("/v1/embeddings");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"data":[{"embedding":[0.1]}]}"#);
    });
    let native = server.mock(|when, then| {
        when.method(POST).path("/api/embed");
        then.status(200).body(r#"{"embeddings":[[0.1]]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["all-minilm"]);
    let (status, _, _) = send(
        state,
        json_req(
            Method::POST,
            "/v1/embeddings",
            json!({"model": "all-minilm", "input": ["hello", "world"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    openai.assert();
    assert_eq!(native.calls(), 0);
}

#[tokio::test]
async fn openai_retry_on_503_excludes_failed_node() {
    let a = MockServer::start();
    let c = MockServer::start();
    let fail = a.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(503).body(r#"{"error":{"message":"busy"}}"#);
    });
    let ok = c.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"cmpl-1"}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &a.base_url(), 8.0, 1, None),
        node("node-c", &c.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "node-a", &["llama3.2:3b"]);
    mark_ready(&state, "node-c", &["llama3.2:3b"]);
    let (status, headers, _) = send(
        state,
        json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
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

async fn spawn_broken_sse_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head).await;
                let _ = sock.write_all(br#"data: {"id":"x"}"#).await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn openai_mid_stream_failure_credits_fail_streak_without_retry() {
    let (broken, broken_h) = spawn_broken_sse_upstream().await;
    let other = MockServer::start();
    let other_hit = other.mock(|when, then| {
        when.method(POST).path("/v1/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"cmpl-1"}"#);
    });
    let state = state_from(fleet_config(vec![
        node("node-a", &broken, 8.0, 1, None),
        node("node-b", &other.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "node-a", &["llama3.2:3b"]);
    mark_ready(&state, "node-b", &["llama3.2:3b"]);
    let a = nid("node-a");
    state.registry.mark_request_failure(&a);
    state.registry.mark_request_failure(&a);
    assert_eq!(state.registry.fail_streak(&a), 2);

    let response = make_app(state.clone())
        .oneshot(json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
        ))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await;
    assert_eq!(state.registry.fail_streak(&a), 3);
    assert!(!state.registry.get(&a).unwrap().healthy);
    assert_eq!(other_hit.calls(), 0);
    broken_h.abort();
}

#[tokio::test]
async fn openai_capacity_miss_uses_error_envelope_and_retry_after() {
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
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
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
    let error = parsed["error"].as_object().expect("openai error object");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("no_healthy_nodes"));
    assert_eq!(error["type"], "server_error");
    assert_eq!(error["code"], "no_healthy_nodes");
}

#[tokio::test]
async fn openai_exhausted_retry_is_openai_shaped_502() {
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", "http://127.0.0.1:1", 24.0, 1, None)],
        timeouts: TimeoutsConfig {
            connect_seconds: 0.2,
            ..TimeoutsConfig::default()
        },
        ..RouterConfig::default()
    });
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/v1/chat/completions",
            openai_chat("llama3.2:3b"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let error = parsed["error"].as_object().expect("openai error object");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("upstream unavailable"));
    assert_eq!(error["type"], "server_error");
    assert_eq!(error["code"], "upstream_unavailable");
}

#[tokio::test]
async fn native_chat_capacity_miss_stays_ollama_string() {
    let state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    let (status, _, body) = send(
        state,
        json_req(
            Method::POST,
            "/api/chat",
            json!({"model": "llama3.2:3b", "messages": []}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("no_healthy_nodes"));
}

#[tokio::test]
async fn openai_retrieve_model_from_aggregated_union() {
    let state = state_from(fleet_config(vec![
        node("a", "http://127.0.0.1:9", 8.0, 1, None),
        node("b", "http://127.0.0.1:10", 24.0, 1, None),
    ]));
    mark_ready(&state, "a", &["llama3.2:3b"]);
    mark_ready(&state, "b", &["llama3.1:8b"]);
    let (status, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/v1/models/llama3.2:3b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["id"], "llama3.2:3b");
    assert_eq!(parsed["object"], "model");

    let (missing, _, body) = send(
        state,
        Request::builder()
            .uri("/v1/models/missing:7b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["error"]["code"], "model_not_found");
    assert_eq!(parsed["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn unsupported_mutate_is_501_without_upstream() {
    let server = MockServer::start();
    let push = server.mock(|when, then| {
        when.method(POST).path("/api/push");
        then.status(200).body("{}");
    });
    let copy = server.mock(|when, then| {
        when.method(POST).path("/api/copy");
        then.status(200).body("{}");
    });
    let create = server.mock(|when, then| {
        when.method(POST).path("/api/create");
        then.status(200).body("{}");
    });
    let blobs = server.mock(|when, then| {
        when.method(POST).path("/api/blobs/sha256-dead");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    for path in [
        "/api/push",
        "/api/copy",
        "/api/create",
        "/api/blobs/sha256-dead",
    ] {
        let (status, _, body) = send(
            state.clone(),
            json_req(Method::POST, path, json!({"model": "llama3.2:3b"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{path}");
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            parsed["error"]
                .as_str()
                .unwrap()
                .contains("not_a_fleet_operation"),
            "{path}"
        );
    }
    assert_eq!(push.calls(), 0);
    assert_eq!(copy.calls(), 0);
    assert_eq!(create.calls(), 0);
    assert_eq!(blobs.calls(), 0);
}

#[tokio::test]
async fn api_show_is_generic_metadata_and_not_idle() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/api/show");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"modelfile":"FROM llama"}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.1:70b"]);
    let gpu = nid("gpu");
    let (status, headers, _) = send(
        state.clone(),
        json_req(Method::POST, "/api/show", json!({"name": "llama3.1:70b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("generic")
    );
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(state.registry.inflight(&gpu), 0);
    mock.assert();
}

#[tokio::test]
async fn unknown_openai_path_is_404_without_inflight() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/v1/images/generations");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let gpu = nid("gpu");
    let (status, _, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/v1/images/generations",
            json!({"model": "llama3.2:3b", "prompt": "x"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["error"]["code"], "unknown_path");
    assert_eq!(parsed["error"]["type"], "invalid_request_error");
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn aggregated_ps_two_holders_two_rows() {
    let a = MockServer::start();
    let b = MockServer::start();
    let state = state_from(fleet_config(vec![
        node("a", &a.base_url(), 8.0, 1, None),
        node("b", &b.base_url(), 8.0, 1, None),
    ]));
    mark_ready(&state, "a", &["qwen3:8b"]);
    mark_ready(&state, "b", &["qwen3:8b"]);
    state.registry.update_ps_from_records(
        &nid("a"),
        [(
            "qwen3:8b",
            PsRecord {
                digest: "aaaaaaaaaaaa".into(),
                size: Some(100),
                size_vram: Some(80),
                details: None,
                expires_at: None,
                context_length: None,
            },
        )],
        Some(1.0),
    );
    state.registry.update_ps_from_records(
        &nid("b"),
        [(
            "qwen3:8b",
            PsRecord {
                digest: "bbbbbbbbbbbb".into(),
                size: Some(100),
                size_vram: Some(90),
                details: None,
                expires_at: None,
                context_length: None,
            },
        )],
        Some(1.0),
    );
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/api/ps")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let models = parsed["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let nodes: Vec<&str> = models
        .iter()
        .map(|m| m["details"]["router_node"].as_str().unwrap())
        .collect();
    assert!(nodes.contains(&"a"));
    assert!(nodes.contains(&"b"));
    assert!(models
        .iter()
        .all(|m| m["digest"].as_str().unwrap().len() >= 12));
}

#[tokio::test]
async fn aggregated_ps_omits_unhealthy() {
    let a = MockServer::start();
    let b = MockServer::start();
    let state = state_from(fleet_config(vec![
        node("a", &a.base_url(), 8.0, 1, None),
        node("b", &b.base_url(), 8.0, 1, None),
    ]));
    mark_ready(&state, "a", &["llama3.2:1b"]);
    state.registry.update_models(&nid("b"), ["llama3.2:1b"]);
    state
        .registry
        .update_ps_state(&nid("a"), ["llama3.2:1b"], Some(1.0));
    state
        .registry
        .update_ps_state(&nid("b"), ["llama3.2:1b"], Some(1.0));
    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/api/ps")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let models = parsed["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["details"]["router_node"], "a");
}

#[tokio::test]
async fn api_show_hits_holder_not_non_holder() {
    let gpu = MockServer::start();
    let cpu = MockServer::start();
    let gpu_show = gpu.mock(|when, then| {
        when.method(POST).path("/api/show");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"modelfile":"FROM llama"}"#);
    });
    let cpu_show = cpu.mock(|when, then| {
        when.method(POST).path("/api/show");
        then.status(200).body(r#"{"modelfile":"wrong"}"#);
    });
    let state = state_from(fleet_config(vec![
        node("gpu", &gpu.base_url(), 80.0, 1, None),
        node("cpu", &cpu.base_url(), 0.0, 0, None),
    ]));
    mark_ready(&state, "gpu", &["llama3.1:70b"]);
    mark_ready(&state, "cpu", &["llama3.2:1b"]);
    let (status, headers, _) = send(
        state.clone(),
        json_req(Method::POST, "/api/show", json!({"name": "llama3.1:70b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("generic")
    );
    assert_eq!(gpu_show.calls(), 1);
    assert_eq!(cpu_show.calls(), 0);
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
    assert_eq!(state.registry.inflight(&nid("gpu")), 0);
}

#[tokio::test]
async fn api_show_miss_is_model_missing() {
    let server = MockServer::start();
    let show = server.mock(|when, then| {
        when.method(POST).path("/api/show");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let (status, _, body) = send(
        state,
        json_req(Method::POST, "/api/show", json!({"name": "missing:7b"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let err = parsed["error"].as_str().unwrap_or("");
    assert!(err.contains("model_missing"), "{err}");
    assert_eq!(show.calls(), 0);
}

#[tokio::test]
async fn api_show_unknown_vram_holder_not_capacity_gated() {
    let server = MockServer::start();
    let show = server.mock(|when, then| {
        when.method(POST).path("/api/show");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"modelfile":"FROM llama"}"#);
    });
    let state = state_from(fleet_config(vec![NodeConfig {
        id: nid("unknown"),
        url: Some(server.base_url()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: None,
            ram_gb: Some(32.0),
            gpus: None,
            cpu_cores: Some(8),
        },
        max_inflight: None,
    }]));
    mark_ready(&state, "unknown", &["llama3.1:70b"]);
    let (status, headers, _) = send(
        state.clone(),
        json_req(Method::POST, "/api/show", json!({"model": "llama3.1:70b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("x-ollama-router-class")
            .and_then(|v| v.to_str().ok()),
        Some("generic")
    );
    show.assert();
    assert!(state
        .registry
        .last_client_request_at(&nid("unknown"))
        .is_none());
}

#[tokio::test]
async fn fleet_pull_targets_both_eligible_gpus() {
    let a = MockServer::start();
    let b = MockServer::start();
    let pull_a = mock_pull(&a, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let pull_b = mock_pull(&b, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    mock_empty_tags(&a);
    mock_empty_tags(&b);
    let state = state_from(fleet_config(vec![
        node("gpu-a", &a.base_url(), 24.0, 1, None),
        node("gpu-b", &b.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "gpu-a", &[]);
    mark_ready(&state, "gpu-b", &[]);
    let gpu = nid("gpu-a");
    let (status, headers, body) = send(
        state.clone(),
        json_req(Method::POST, "/api/pull", json!({"model": "qwen3:8b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.lines().any(|l| l.contains("\"status\":\"success\"")));
    assert_eq!(pull_a.calls(), 1);
    assert_eq!(pull_b.calls(), 1);
    assert!(state.registry.last_client_request_at(&gpu).is_none());
}

#[tokio::test]
async fn fleet_pull_large_against_cpu_and_unknown_is_503() {
    let cpu = MockServer::start();
    let unknown = MockServer::start();
    let cpu_pull = mock_pull(&cpu, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let unknown_pull = mock_pull(&unknown, Duration::ZERO, 200, "{\"status\":\"success\"}\n");
    let state = state_from(fleet_config(vec![
        node("cpu", &cpu.base_url(), 0.0, 0, None),
        NodeConfig {
            id: nid("unknown"),
            url: Some(unknown.base_url()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: None,
                ram_gb: Some(32.0),
                gpus: None,
                cpu_cores: Some(8),
            },
            max_inflight: None,
        },
    ]));
    mark_ready(&state, "cpu", &[]);
    mark_ready(&state, "unknown", &[]);
    let (status, _, body) = send(
        state,
        json_req(Method::POST, "/api/pull", json!({"model": "llama3.1:70b"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let err = parsed["error"].as_str().unwrap_or("");
    assert!(err.contains("insufficient_capacity"), "{err}");
    assert!(!err.contains("\"status\":\"success\""));
    assert_eq!(cpu_pull.calls(), 0);
    assert_eq!(unknown_pull.calls(), 0);
}

#[tokio::test]
async fn api_version_is_router_owned() {
    let server = MockServer::start();
    let version = server.mock(|when, then| {
        when.method(GET).path("/api/version");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"version":"ollama-upstream-9.9.9"}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:1b"]);
    let gpu = nid("gpu");

    let (hz_status, _, hz_body) = send(
        state.clone(),
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(hz_status, StatusCode::OK);
    let hz: Value = serde_json::from_slice(&hz_body).unwrap();

    let (status, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/api/version")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["version"], hz["version"]);
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(version.calls(), 0);
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(state.registry.inflight(&gpu), 0);
}

fn mock_delete<'a>(server: &'a MockServer, status: u16, body: &str) -> httpmock::Mock<'a> {
    let body = body.to_string();
    server.mock(|when, then| {
        when.method(DELETE).path("/api/delete");
        then.status(status)
            .header("content-type", "application/json")
            .body(body);
    })
}

#[tokio::test]
async fn fleet_delete_succeeds_when_upstream_holds_model() {
    let server = MockServer::start();
    let delete = mock_delete(&server, 200, r#"{"status":"success"}"#);
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
    let gpu = nid("gpu");
    let (status, headers, body) = send(
        state.clone(),
        json_req(
            Method::DELETE,
            "/api/delete",
            json!({"model": "llama3.2:3b"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("ndjson"))
        .collect();
    assert!(
        lines.iter().any(|l| l["status"] == "success"),
        "expected success line in {lines:?}"
    );
    assert_eq!(delete.calls(), 1);
    assert!(state.registry.last_client_request_at(&gpu).is_none());
}

#[tokio::test]
async fn fleet_delete_already_absent_is_success_ndjson() {
    let state = state_from(fleet_config(vec![node(
        "gpu",
        "http://127.0.0.1:9",
        8.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &[]);
    let (status, headers, body) = send(
        state,
        json_req(Method::DELETE, "/api/delete", json!({"model": "gone:1b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("\"status\":\"success\""));
    assert!(!text.contains("\"error\""));
}

#[tokio::test]
async fn fleet_delete_targets_both_holders() {
    let a = MockServer::start();
    let b = MockServer::start();
    let delete_a = mock_delete(&a, 200, r#"{"status":"success"}"#);
    let delete_b = mock_delete(&b, 200, r#"{"status":"success"}"#);
    for server in [&a, &b] {
        server.mock(|when, then| {
            when.method(GET).path("/api/tags");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"models":[{"name":"qwen3:8b"}]}"#);
        });
    }
    let state = state_from(fleet_config(vec![
        node("gpu-a", &a.base_url(), 24.0, 1, None),
        node("gpu-b", &b.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "gpu-a", &["qwen3:8b"]);
    mark_ready(&state, "gpu-b", &["qwen3:8b"]);
    let gpu = nid("gpu-a");
    let (status, headers, body) = send(
        state.clone(),
        json_req(Method::DELETE, "/api/delete", json!({"model": "qwen3:8b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.lines().any(|l| l.contains("\"status\":\"success\"")));
    assert_eq!(delete_a.calls(), 1);
    assert_eq!(delete_b.calls(), 1);
    assert!(state.registry.last_client_request_at(&gpu).is_none());
}

#[tokio::test]
async fn delete_missing_model_is_400() {
    let state = state_from(RouterConfig::default());
    let (status, headers, body) =
        send(state, json_req(Method::DELETE, "/api/delete", json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_ne!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/x-ndjson")
    );
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("model is required"));
}

#[tokio::test]
async fn wrong_method_on_known_ollama_path_is_405_without_upstream() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/tags");
        then.status(200).body(r#"{"models":[]}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["llama3.2:3b"]);
    let gpu = nid("gpu");
    let (status, _, body) = send(
        state.clone(),
        json_req(Method::POST, "/api/tags", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("method not allowed"));
    assert!(state.registry.last_client_request_at(&gpu).is_none());
    assert_eq!(state.registry.inflight(&gpu), 0);
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn wrong_method_on_known_openai_path_is_405_without_upstream() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(GET).path("/v1/chat/completions");
        then.status(200).body("{}");
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
        Request::builder()
            .uri("/v1/chat/completions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["error"]["code"], "method_not_allowed");
    assert_eq!(parsed["error"]["type"], "invalid_request_error");
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn unknown_ollama_path_is_404_without_upstream() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(GET).path("/api/not-a-real-endpoint");
        then.status(200).body("{}");
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
        Request::builder()
            .uri("/api/not-a-real-endpoint")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"].as_str().unwrap().contains("unknown path"));
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("[reason: unknown_path]"));
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn openai_model_delete_and_fine_tuning_are_501_without_upstream() {
    let server = MockServer::start();
    let delete_hit = server.mock(|when, then| {
        when.method(DELETE).path("/v1/models/qwen3:8b");
        then.status(200).body("{}");
    });
    let ft_hit = server.mock(|when, then| {
        when.method(POST).path("/v1/fine_tuning/jobs");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3:8b"]);

    let (del_status, _, del_body) = send(
        state.clone(),
        Request::builder()
            .method(Method::DELETE)
            .uri("/v1/models/qwen3:8b")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(del_status, StatusCode::NOT_IMPLEMENTED);
    let parsed: Value = serde_json::from_slice(&del_body).unwrap();
    assert_eq!(parsed["error"]["code"], "not_a_fleet_operation");

    let (ft_status, _, ft_body) = send(
        state,
        json_req(
            Method::POST,
            "/v1/fine_tuning/jobs",
            json!({"model": "qwen3:8b"}),
        ),
    )
    .await;
    assert_eq!(ft_status, StatusCode::NOT_IMPLEMENTED);
    let parsed: Value = serde_json::from_slice(&ft_body).unwrap();
    assert_eq!(parsed["error"]["code"], "not_a_fleet_operation");
    assert_eq!(delete_hit.calls(), 0);
    assert_eq!(ft_hit.calls(), 0);
}

#[tokio::test]
async fn api_stop_fans_out_to_every_loaded_holder() {
    let a = MockServer::start();
    let b = MockServer::start();
    let a_unload = a.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"done":true,"done_reason":"unload"}"#);
    });
    let b_unload = b.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"done":true,"done_reason":"unload"}"#);
    });
    let state = state_from(fleet_config(vec![
        node("a", &a.base_url(), 24.0, 1, None),
        node("b", &b.base_url(), 24.0, 1, None),
    ]));
    mark_ready(&state, "a", &["qwen3:8b"]);
    mark_ready(&state, "b", &["qwen3:8b"]);
    state
        .registry
        .update_ps_state(&nid("a"), ["qwen3:8b"], Some(4.0));
    state
        .registry
        .update_ps_state(&nid("b"), ["qwen3:8b"], Some(4.0));

    let (status, _, body) = send(
        state,
        json_req(Method::POST, "/api/stop", json!({"model": "qwen3:8b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["done"], true);
    assert_eq!(parsed["done_reason"], "unload");
    a_unload.assert();
    b_unload.assert();
}

#[tokio::test]
async fn api_stop_of_unloaded_model_is_success() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["other:1b"]);
    let (status, _, body) = send(
        state,
        json_req(Method::POST, "/api/stop", json!({"model": "gone:1b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["done_reason"], "unload");
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn api_stop_missing_model_is_400_without_upstream() {
    let server = MockServer::start();
    let hit = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body("{}");
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3:8b"]);
    let (status, _, body) = send(state, json_req(Method::POST, "/api/stop", json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("model is required"));
    assert_eq!(hit.calls(), 0);
}

#[tokio::test]
async fn api_stop_does_not_set_last_client_request_at() {
    let server = MockServer::start();
    let _unload = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body(r#"{"done":true}"#);
    });
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        24.0,
        1,
        None,
    )]));
    mark_ready(&state, "gpu", &["qwen3:8b"]);
    state
        .registry
        .update_ps_state(&nid("gpu"), ["qwen3:8b"], Some(4.0));
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
    let (status, _, _) = send(
        state.clone(),
        json_req(Method::POST, "/api/stop", json!({"model": "qwen3:8b"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
}

async fn metrics_text(state: AppState) -> String {
    let (_, _, body) = send(
        state,
        Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn local_rejections_increment_requests_and_route_reason() {
    let state = state_from(RouterConfig::default());
    let (status, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/api/not-real")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let text = metrics_text(state).await;
    assert!(
        text.contains("code=\"404\"") && text.contains("node=\"-\""),
        "{text}"
    );
    assert!(
        text.contains("ollama_router_route_reason_total{reason=\"unknown_compat_path\"}"),
        "{text}"
    );

    let state = state_from(RouterConfig::default());
    let (status, _, _) = send(
        state.clone(),
        Request::builder()
            .uri("/v1/chat/completions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let text = metrics_text(state).await;
    assert!(
        text.contains("ollama_router_route_reason_total{reason=\"method_not_allowed\"}"),
        "{text}"
    );
}

#[tokio::test]
async fn pull_missing_model_records_metrics() {
    let state = state_from(RouterConfig::default());
    let (status, _, _) = send(
        state.clone(),
        json_req(Method::POST, "/api/pull", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let text = metrics_text(state).await;
    assert!(
        text.contains("ollama_router_route_reason_total{reason=\"model_required\"}"),
        "{text}"
    );
    assert!(
        text.contains("code=\"400\"") && text.contains("class=\"pull\""),
        "{text}"
    );
}

#[tokio::test]
async fn readyz_503_when_all_healthy_nodes_saturated() {
    let server = MockServer::start();
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        Some(1),
    )]));
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);
    state.registry.inflight_inc(&nid("gpu"));

    let (ready, _, body) = send(
        state.clone(),
        Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(ready, StatusCode::SERVICE_UNAVAILABLE);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ready"], false);
    assert!(parsed["reason"].as_str().unwrap().contains("saturated"));

    let (health, _, body) = send(
        state,
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(health, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "ok");
}

#[tokio::test]
async fn readyz_200_when_healthy_node_has_headroom() {
    let server = MockServer::start();
    let state = state_from(fleet_config(vec![node(
        "gpu",
        &server.base_url(),
        8.0,
        1,
        Some(2),
    )]));
    mark_ready(&state, "gpu", &["qwen3-embedding:8b"]);
    state.registry.inflight_inc(&nid("gpu"));

    let (status, _, body) = send(
        state,
        Request::builder()
            .uri("/readyz")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ready"], true);
}
