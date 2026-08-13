use anyhow::Context;
use clap::Parser;
use ollama_node_agent::cli::{Cli, Commands};
use ollama_node_agent::config::AgentConfig;
use ollama_node_agent::listen::{format_host_port, resolve_bind, AddrSource, HostAddrs};
use ollama_node_agent::setup::{SetupContext, SetupPaths};

fn init_tracing() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ollama_node_agent=info".into()),
        )
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Serve {
            config,
            host,
            port,
            windows_service: _,
        } => {
            init_tracing();
            let mut cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            if let Some(h) = host {
                cfg.listen = ollama_node_agent::config::BindSpec::Address(h);
            }
            if let Some(p) = port {
                cfg.port = p;
            }
            let token_set = cfg.bearer_token().is_some();
            let addrs = HostAddrs;
            let addrs: &dyn AddrSource = &addrs;
            let agent_ip = resolve_bind(&cfg.listen, addrs, token_set)?;
            let ollama_ip = resolve_bind(&cfg.ollama.listen, addrs, token_set)?;
            let bind = std::net::SocketAddr::new(agent_ip, cfg.port);
            let ollama_listen = format_host_port(ollama_ip, 11434);
            ollama_node_agent::http::serve(cfg, bind, ollama_listen).await
        }
        Commands::Setup { config, ts_authkey } => {
            init_tracing();
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            let paths = SetupPaths::for_os();
            let key = ts_authkey.filter(|s| !s.trim().is_empty());
            if key.is_some() && !cfg.tailscale.enable {
                tracing::info!("TS_AUTHKEY present but tailscale.enable is false; skip join");
            }
            let ctx = SetupContext {
                config: &cfg,
                paths: &paths,
                ts_authkey: key.as_deref(),
                dry_commands: false,
            };
            let state = ollama_node_agent::setup::run(ctx).await?;
            tracing::info!(installed = state.ollama_installed, "setup complete");
            Ok(())
        }
        Commands::Doctor { config } => {
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            let report = ollama_node_agent::doctor::run(&cfg).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Commands::Uninstall {
            config,
            purge_ollama,
        } => {
            init_tracing();
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            ollama_node_agent::uninstall::run(&cfg, purge_ollama).await
        }
    }
}
