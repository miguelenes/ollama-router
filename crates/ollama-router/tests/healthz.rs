//! End-to-end HTTP tests for the skeleton server.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use ollama_router::http::{make_app, AppState};
use ollama_router_core::RouterConfig;
use serde_json::Value;
use tower::ServiceExt;

async fn get(path: &str) -> (StatusCode, Value) {
    let state = AppState::from_config(RouterConfig::default()).expect("state");
    let response = make_app(state)
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn healthz_is_open_and_ok() {
    let (status, json) = get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert!(json["version"].as_str().is_some_and(|v| !v.is_empty()));
}

#[tokio::test]
async fn metrics_is_open_prometheus_text() {
    let state = AppState::from_config(RouterConfig::default()).expect("state");
    let response = make_app(state)
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/plain"), "{ct}");
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok());
    assert!(request_id.is_some_and(|id| !id.is_empty()));
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ollama_router_"), "{text}");
}
