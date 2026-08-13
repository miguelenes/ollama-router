use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use ollama_router::cli::{Cli, Commands};
use ollama_router::http::{make_app, AppState};
use ollama_router_core::load_config;

fn not_implemented(command: &str) -> ! {
    eprintln!("error: {command} is not implemented yet");
    std::process::exit(2);
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ollama_router=info".into()),
        )
        .init();
}

async fn serve(host: String, port: u16, config: Option<PathBuf>) -> anyhow::Result<()> {
    init_tracing();
    let loaded = load_config(config.as_deref()).context("load config")?;
    let state = AppState::from_config(loaded).context("build app state")?;
    let app = make_app(state);
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(listen.addr = %addr, "ollama-router started");
    axum::serve(listener, app).await.context("server")?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve { host, port, config } => serve(host, port, config).await,
        Commands::Ensure { .. } => not_implemented("ensure"),
        Commands::Delete { .. } => not_implemented("delete"),
        Commands::Nodes { .. } => not_implemented("nodes"),
        Commands::Provision { .. } => not_implemented("provision"),
    }
}
