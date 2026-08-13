//! Admin bearer + ensure 202 / GET job / wait timeout.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use httpmock::prelude::*;
use ollama_router::http::{make_app, AppState};
use ollama_router_core::config::{Capacity, NodeConfig, RouterConfig};
use ollama_router_core::fleet::NodeId;
use serde_json::{json, Value};
use tower::ServiceExt;

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("node id")
}

fn node(id: &str, url: &str, vram: f64) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: Some(url.trim_end_matches('/').to_string()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: Some(vram),
            ram_gb: Some(32.0),
            gpus: Some(1),
            cpu_cores: Some(8),
        },
        max_inflight: None,
        ssh: None,
        provision: None,
    }
}

fn state_with_token(config: RouterConfig, token: Option<&str>) -> AppState {
    let mut state = AppState::from_config(config).expect("state");
    state.admin_token = token.map(str::to_string);
    state
}

async fn send(state: AppState, req: Request<Body>) -> (StatusCode, bytes::Bytes) {
    let response = make_app(state).oneshot(req).await.expect("oneshot");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, body)
}

fn json_req(method: Method, path: &str, body: Value, token: Option<&str>) -> Request<Body> {
    let bytes = serde_json::to_vec(&body).expect("json");
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(bytes)).expect("request")
}

#[tokio::test]
async fn admin_ensure_forbidden_without_token() {
    let state = state_with_token(RouterConfig::default(), None);
    let (status, body) = send(
        state,
        json_req(
            Method::POST,
            "/router/v1/models/ensure",
            json!({"models": ["moondream"]}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["error"]
        .as_str()
        .unwrap()
        .contains("OLLAMA_ROUTER_ADMIN_TOKEN"));
}

#[tokio::test]
async fn admin_ensure_202_and_get_job() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"moondream"}]}"#);
    });
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0)],
        ..RouterConfig::default()
    };
    let state = state_with_token(config, Some("secret"));
    state.registry.set_healthy(&nid("gpu"));
    let (status, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/router/v1/models/ensure",
            json!({"models": ["moondream"]}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let job_id = parsed["job_id"].as_str().expect("job_id");
    assert!(!job_id.is_empty());

    let (get_status, get_body) = send(
        state,
        Request::builder()
            .uri(format!("/router/v1/jobs/{job_id}"))
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    let job: Value = serde_json::from_slice(&get_body).unwrap();
    assert_eq!(job["id"], job_id);
}

#[tokio::test]
async fn admin_wait_timeout_stays_202() {
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
            .delay(std::time::Duration::from_secs(2))
            .body("{\"status\":\"success\"}\n");
    });
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0)],
        ensure_wait_max_seconds: 0.05,
        ..RouterConfig::default()
    };
    let state = state_with_token(config, Some("secret"));
    state.registry.set_healthy(&nid("gpu"));
    let (status, body) = send(
        state,
        json_req(
            Method::POST,
            "/router/v1/models/ensure?wait=true",
            json!({"models": ["moondream"]}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["wait_timeout_seconds"].as_f64().is_some());
    assert!(parsed["job_id"].as_str().is_some());
}

#[tokio::test]
async fn provision_forbidden_without_token() {
    let mut state = state_with_token(RouterConfig::default(), None);
    state.provisioner = None;
    let (status, _) = send(
        state,
        json_req(
            Method::POST,
            "/router/v1/nodes/provision",
            json!({}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn provision_empty_set_ok_with_token() {
    let state = state_with_token(RouterConfig::default(), Some("secret"));
    let (status, body) = send(
        state,
        json_req(
            Method::POST,
            "/router/v1/nodes/provision",
            json!({"dry_run": true}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["results"], json!([]));
}

#[tokio::test]
async fn verda_routes_503_when_disabled() {
    let state = state_with_token(RouterConfig::default(), Some("secret"));
    for path in [
        "/router/v1/verda/status",
        "/router/v1/verda/ensure",
        "/router/v1/verda/destroy",
    ] {
        let method = if path.ends_with("status") {
            Method::GET
        } else {
            Method::POST
        };
        let (status, body) = send(
            state.clone(),
            json_req(method, path, json!({}), Some("secret")),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            parsed["error"]
                .as_str()
                .unwrap_or("")
                .contains("not enabled"),
            "{path}: {parsed}"
        );
    }
}

#[tokio::test]
async fn verda_routes_forbidden_without_token() {
    let state = state_with_token(RouterConfig::default(), None);
    let (status, _) = send(
        state,
        json_req(
            Method::GET,
            "/router/v1/verda/status",
            json!({}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

fn get_req(path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request")
}

#[tokio::test]
async fn new_admin_routes_forbidden_without_token() {
    let state = state_with_token(RouterConfig::default(), None);
    for path in [
        "/router/v1/nodes",
        "/router/v1/models",
        "/router/v1/jobs",
        "/router/v1/stats",
    ] {
        let (status, _) = send(state.clone(), get_req(path, Some("secret"))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    let (status, _) = send(
        state,
        json_req(Method::POST, "/router/v1/reload", json!({}), Some("secret")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn new_admin_routes_ok_with_bearer() {
    let state = state_with_token(RouterConfig::default(), Some("secret"));
    for path in [
        "/router/v1/nodes",
        "/router/v1/models",
        "/router/v1/jobs",
        "/router/v1/stats",
    ] {
        let (status, body) = send(state.clone(), get_req(path, Some("secret"))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{path}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let (status, body) = send(
        state,
        json_req(Method::POST, "/router/v1/reload", json!({}), Some("secret")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[tokio::test]
async fn put_nodes_rejects_public_ipv4_and_leaves_fleet_yaml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fleet = dir.path().join("fleet.yaml");
    let yaml = "version: 1\nnodes:\n  - id: local\n    url: http://127.0.0.1:11434\n    static:\n      vram_gb: 8\n";
    std::fs::write(&fleet, yaml).expect("write fleet");
    let original = std::fs::read(&fleet).expect("read");
    let config = RouterConfig {
        nodes: vec![node("local", "http://127.0.0.1:11434", 8.0)],
        fleet_path: fleet.clone(),
        ..RouterConfig::default()
    };
    let state = state_with_token(config, Some("secret"));
    let (status, body) = send(
        state,
        json_req(
            Method::PUT,
            "/router/v1/nodes",
            json!({"id": "local", "url": "http://8.8.8.8:11434"}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap_or("")
            .contains("public IPv4"),
        "{parsed}"
    );
    assert_eq!(std::fs::read(&fleet).expect("reread"), original);
}

#[tokio::test]
async fn list_jobs_returns_ensure_job() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"moondream"}]}"#);
    });
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0)],
        ..RouterConfig::default()
    };
    let state = state_with_token(config, Some("secret"));
    state.registry.set_healthy(&nid("gpu"));
    let (status, body) = send(
        state.clone(),
        json_req(
            Method::POST,
            "/router/v1/models/ensure",
            json!({"models": ["moondream"]}),
            Some("secret"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let job_id = parsed["job_id"].as_str().expect("job_id");
    let (list_status, list_body) = send(state, get_req("/router/v1/jobs", Some("secret"))).await;
    assert_eq!(list_status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&list_body).unwrap();
    let jobs = listed["jobs"].as_array().expect("jobs");
    assert!(
        jobs.iter().any(|j| j["id"].as_str() == Some(job_id)),
        "{listed}"
    );
}

#[tokio::test]
async fn metrics_and_healthz_need_no_bearer() {
    let state = state_with_token(RouterConfig::default(), Some("secret"));
    let (hz, _) = send(state.clone(), get_req("/healthz", None)).await;
    assert_eq!(hz, StatusCode::OK);
    let (ms, body) = send(state, get_req("/metrics", None)).await;
    assert_eq!(ms, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("ollama_router_") || text.contains("# HELP"),
        "{text}"
    );
}

#[tokio::test]
async fn no_thunder_or_runpod_admin_paths() {
    let state = state_with_token(RouterConfig::default(), Some("secret"));
    for path in [
        "/router/v1/thunder/status",
        "/router/v1/runpod/status",
        "/thunder",
        "/runpod",
    ] {
        let (status, body) = send(state.clone(), get_req(path, Some("secret"))).await;
        let text = String::from_utf8_lossy(&body);
        assert!(
            !text.to_ascii_lowercase().contains("thundercompute")
                && !text.to_ascii_lowercase().contains("runpod not enabled"),
            "{path}: {text}"
        );
        assert_ne!(status, StatusCode::OK, "{path}");
    }
}
