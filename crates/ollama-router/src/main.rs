use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use ollama_router::cli::{Cli, Commands};
use ollama_router::health::{reload_permanent_inventory, run as run_health};
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
    let app = make_app(state.clone());
    tokio::spawn(run_health(state.clone()));
    spawn_sighup_reloader(state.clone());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(listen.addr = %addr, "ollama-router started");
    axum::serve(listener, app).await.context("server")?;
    Ok(())
}

fn spawn_sighup_reloader(state: AppState) {
    #[cfg(unix)]
    tokio::spawn(async move {
        let mut hangup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        {
            Ok(signal) => signal,
            Err(error) => {
                tracing::error!(%error, "failed to register SIGHUP handler");
                return;
            }
        };
        while hangup.recv().await.is_some() {
            if let Err(error) = reload_permanent_inventory(&state) {
                tracing::error!(%error, "SIGHUP fleet reload failed");
            }
        }
    });
    #[cfg(not(unix))]
    let _ = state;
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
