use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ollama_node_agent::config::AgentConfig;
use ollama_node_agent::http::{make_app, AppState};
use ollama_node_agent::metrics::AgentMetrics;
use tokio::sync::RwLock;
use tower::ServiceExt;

fn test_state() -> AppState {
    AppState {
        config: Arc::new(AgentConfig::default()),
        ollama_listen: "127.0.0.1:11434".into(),
        metrics: Arc::new(AgentMetrics::new().expect("metrics")),
        last: Arc::new(RwLock::new(None)),
        cpu_usage_pct: Arc::new(std::sync::RwLock::new(None)),
    }
}

#[tokio::test]
async fn healthz_is_open() {
    let response = make_app(test_state())
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn v1_requires_bearer_when_token_set() {
    let cfg = AgentConfig {
        token: Some("secret".into()),
        ..AgentConfig::default()
    };
    let state = AppState {
        config: Arc::new(cfg),
        ollama_listen: "127.0.0.1:11434".into(),
        metrics: Arc::new(AgentMetrics::new().expect("metrics")),
        last: Arc::new(RwLock::new(None)),
        cpu_usage_pct: Arc::new(std::sync::RwLock::new(None)),
    };
    let response = make_app(state)
        .oneshot(
            Request::builder()
                .uri("/v1/capacity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn capacity_shape_has_required_keys() {
    let response = make_app(test_state())
        .oneshot(
            Request::builder()
                .uri("/v1/capacity")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    for key in [
        "vram_gb",
        "gpus",
        "ram_gb",
        "cpu_cores",
        "hostname",
        "collected_at",
        "gpu_names",
        "agent_version",
        "gpus_detail",
        "vram_used_gb",
        "vram_free_gb",
    ] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    assert!(v.get("pressure_level").is_none());
    let report: ollama_capacity_types::CapacityReport =
        serde_json::from_value(v).expect("deserialize");
    assert!(report.ram_gb > 0.0);
}

#[tokio::test]
async fn pressure_shape_has_required_keys() {
    let response = make_app(test_state())
        .oneshot(
            Request::builder()
                .uri("/v1/pressure")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    for key in ["collected_at", "pressure", "pressure_level", "live"] {
        assert!(v.get(key).is_some(), "missing {key}");
    }
    let level = v["pressure_level"].as_str().unwrap();
    assert!(
        matches!(level, "ok" | "elevated" | "critical" | "unknown"),
        "unexpected level {level}"
    );
}

#[tokio::test]
async fn metrics_is_open() {
    let response = make_app(test_state())
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    for name in [
        "ollama_node_agent_ollama_up",
        "ollama_node_agent_models",
        "ollama_node_agent_gpu_vram_gb",
        "ollama_node_agent_ram_available_gb",
        "ollama_node_agent_gpu_utilization_pct",
        "agent_collect_seconds",
    ] {
        assert!(body.contains(name), "missing metric {name}");
    }
}

#[tokio::test]
async fn old_linux_fixture_still_deserializes() {
    let raw = include_str!("../../ollama-router-core/tests/fixtures/capacity-linux.json");
    let report: ollama_capacity_types::CapacityReport = serde_json::from_str(raw).unwrap();
    assert!((report.vram_gb - 8.0).abs() < 1e-9);
    assert_eq!(report.gpus, 1);
}
