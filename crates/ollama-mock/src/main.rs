//! Tiny plaintext Ollama mock for compose: canned tags + short generate/chat/embed.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MockRole {
    Cpu,
    Gpu,
}

impl MockRole {
    fn from_env() -> Self {
        match std::env::var("MOCK_ROLE")
            .unwrap_or_else(|_| "cpu".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "gpu" => Self::Gpu,
            _ => Self::Cpu,
        }
    }

    fn models(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &["qwen3-embedding:8b", "llama3.2:1b"],
            Self::Gpu => &["qwen3-embedding:8b", "llama3.2:1b", "llama3.2:3b"],
        }
    }

    fn vram_gb(self) -> f64 {
        match self {
            Self::Cpu => 0.0,
            Self::Gpu => 24.0,
        }
    }
}

#[derive(Clone)]
struct MockState {
    models: Arc<Mutex<BTreeSet<String>>>,
}

impl MockState {
    fn new(role: MockRole) -> Self {
        let models = role
            .models()
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        Self {
            models: Arc::new(Mutex::new(models)),
        }
    }

    fn lock_models(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Deserialize)]
struct ModelBody {
    model: Option<String>,
}

#[tokio::main]
async fn main() {
    let role = MockRole::from_env();
    let ollama_port: u16 = std::env::var("OLLAMA_MOCK_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(11434);
    let capacity_port: Option<u16> = std::env::var("CAPACITY_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|p| *p > 0);

    let state = MockState::new(role);
    let ollama = serve_ollama(state.clone(), ollama_port);
    if let Some(port) = capacity_port {
        let capacity = serve_capacity(role, state, port);
        tokio::select! {
            result = ollama => {
                if let Err(error) = result {
                    eprintln!("ollama-mock ollama listener: {error}");
                    std::process::exit(1);
                }
            }
            result = capacity => {
                if let Err(error) = result {
                    eprintln!("ollama-mock capacity listener: {error}");
                    std::process::exit(1);
                }
            }
        }
    } else if let Err(error) = ollama.await {
        eprintln!("ollama-mock ollama listener: {error}");
        std::process::exit(1);
    }
}

async fn serve_ollama(state: MockState, port: u16) -> std::io::Result<()> {
    let app = Router::new()
        .route("/api/tags", get(tags))
        .route("/api/generate", post(generate))
        .route("/api/chat", post(chat))
        .route("/api/embed", post(embed))
        .route("/api/embeddings", post(embed))
        .route("/api/pull", post(pull))
        .with_state(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

async fn serve_capacity(role: MockRole, ollama: MockState, port: u16) -> std::io::Result<()> {
    let app = Router::new()
        .route("/v1/capacity", get(capacity))
        .route("/v1/pressure", get(pressure))
        .route("/metrics", get(agent_metrics))
        .with_state(CapacityState { role, ollama });
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

#[derive(Clone)]
struct CapacityState {
    role: MockRole,
    ollama: MockState,
}

async fn tags(State(state): State<MockState>) -> Json<Value> {
    let models: Vec<Value> = state
        .lock_models()
        .iter()
        .map(|name| json!({"name": name, "model": name}))
        .collect();
    Json(json!({"models": models}))
}

async fn pull(State(state): State<MockState>, Json(body): Json<ModelBody>) -> Response {
    let Some(model) = body.model.filter(|name| !name.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "model is required"})),
        )
            .into_response();
    };
    state.lock_models().insert(model);
    (
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        "{\"status\":\"success\"}\n",
    )
        .into_response()
}

async fn generate(Json(body): Json<ModelBody>) -> Response {
    let model = body.model.unwrap_or_else(|| "llama3.2:1b".into());
    let chunk = format!(
        "{}\n{}\n",
        json!({"model": model, "response": "ok", "done": false}),
        json!({"model": model, "response": "", "done": true})
    );
    (
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        chunk,
    )
        .into_response()
}

async fn chat(Json(body): Json<ModelBody>) -> Response {
    let model = body.model.unwrap_or_else(|| "llama3.2:1b".into());
    let chunk = format!(
        "{}\n{}\n",
        json!({"model": model, "message": {"role": "assistant", "content": "ok"}, "done": false}),
        json!({"model": model, "message": {"role": "assistant", "content": ""}, "done": true})
    );
    (
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        chunk,
    )
        .into_response()
}

async fn embed(Json(body): Json<ModelBody>) -> Json<Value> {
    let model = body.model.unwrap_or_else(|| "qwen3-embedding:8b".into());
    Json(json!({
        "model": model,
        "embeddings": [[0.1, 0.2, 0.3]]
    }))
}

async fn capacity(State(state): State<CapacityState>) -> Json<Value> {
    Json(json!({
        "pressure_level": "ok",
        "vram_gb": state.role.vram_gb(),
    }))
}

async fn pressure(State(state): State<CapacityState>) -> Json<Value> {
    Json(json!({
        "pressure_level": "ok",
        "vram_gb": state.role.vram_gb(),
    }))
}

async fn agent_metrics(State(state): State<CapacityState>) -> Response {
    let n = state.ollama.lock_models().len();
    let vram = state.role.vram_gb();
    let body = format!(
        "# HELP ollama_up 1 if GET /api/tags succeeded\n\
         # TYPE ollama_up gauge\n\
         ollama_up 1\n\
         # HELP ollama_models On-disk model count from GET /api/tags (no names)\n\
         # TYPE ollama_models gauge\n\
         ollama_models {n}\n\
         # HELP ollama_gpu_vram_gb Sum of GPU VRAM (GiB)\n\
         # TYPE ollama_gpu_vram_gb gauge\n\
         ollama_gpu_vram_gb {vram}\n\
         # HELP ram_available_gb Available RAM (GiB)\n\
         # TYPE ram_available_gb gauge\n\
         ram_available_gb 0\n\
         # HELP gpu_utilization_pct Mean GPU utilization percent\n\
         # TYPE gpu_utilization_pct gauge\n\
         gpu_utilization_pct 0\n"
    );
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}
