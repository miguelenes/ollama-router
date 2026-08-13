use anyhow::Context;
use clap::Parser;
use ollama_router::cli::{Cli, Commands};
use ollama_router::http::make_app;

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

async fn serve(host: String, port: u16) -> anyhow::Result<()> {
    init_tracing();
    let app = make_app();
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
        Commands::Serve {
            host,
            port,
            config: _,
        } => serve(host, port).await,
        Commands::Ensure { .. } => not_implemented("ensure"),
        Commands::Delete { .. } => not_implemented("delete"),
        Commands::Nodes { .. } => not_implemented("nodes"),
        Commands::Provision { .. } => not_implemented("provision"),
    }
}
