//! Tiny plaintext Ollama mock for compose: canned tags + short generate/chat/embed.

use std::net::SocketAddr;

use axum::extract::State;
use axum::http::StatusCode;
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

    let ollama = serve_ollama(role, ollama_port);
    if let Some(port) = capacity_port {
        let capacity = serve_capacity(role, port);
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

async fn serve_ollama(role: MockRole, port: u16) -> std::io::Result<()> {
    let app = Router::new()
        .route("/api/tags", get(tags))
        .route("/api/generate", post(generate))
        .route("/api/chat", post(chat))
        .route("/api/embed", post(embed))
        .route("/api/embeddings", post(embed))
        .with_state(role);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

async fn serve_capacity(role: MockRole, port: u16) -> std::io::Result<()> {
    let app = Router::new()
        .route("/v1/capacity", get(capacity))
        .route("/v1/pressure", get(pressure))
        .with_state(role);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

async fn tags(State(role): State<MockRole>) -> Json<Value> {
    let models: Vec<Value> = role
        .models()
        .iter()
        .map(|name| json!({"name": name, "model": name}))
        .collect();
    Json(json!({"models": models}))
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

async fn capacity(State(role): State<MockRole>) -> Json<Value> {
    Json(json!({
        "pressure_level": "ok",
        "vram_gb": role.vram_gb(),
    }))
}

async fn pressure(State(role): State<MockRole>) -> Json<Value> {
    Json(json!({
        "pressure_level": "ok",
        "vram_gb": role.vram_gb(),
    }))
}
