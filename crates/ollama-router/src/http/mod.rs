//! Axum app: `/healthz` now; `/readyz`, `/metrics`, and admin later.

use axum::{routing::get, Json, Router};
use serde::Serialize;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

pub fn make_app() -> Router {
    Router::new().route("/healthz", get(healthz))
}
