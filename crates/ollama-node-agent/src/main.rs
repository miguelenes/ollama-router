use anyhow::Context;
use clap::Parser;
use ollama_node_agent::cli::{Cli, Commands};
use ollama_node_agent::config::AgentConfig;
use ollama_node_agent::setup::{SetupContext, SetupPaths};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    #[cfg(windows)]
    {
        if let Commands::Serve {
            config,
            host,
            port,
            windows_service: true,
        } = cli.command
        {
            return ollama_node_agent::windows_scm::run(
                ollama_node_agent::windows_scm::ServeOpts { config, host, port },
            );
        }
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?
        .block_on(async_main(cli.command))
}

async fn async_main(command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Serve {
            config,
            host,
            port,
            windows_service: _,
        } => {
            ollama_node_agent::init_tracing();
            let (cfg, bind, ollama_listen) =
                ollama_node_agent::http::prepare_serve(config.as_deref(), host, port)?;
            ollama_node_agent::http::serve(cfg, bind, ollama_listen).await
        }
        Commands::Setup {
            print_unit: true, ..
        } => {
            print!("{}", ollama_node_agent::setup::agent_unit_text());
            Ok(())
        }
        Commands::Setup {
            config,
            ts_authkey,
            print_unit: false,
        } => {
            ollama_node_agent::init_tracing();
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
            ollama_node_agent::init_tracing();
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            ollama_node_agent::uninstall::run(&cfg, purge_ollama).await
        }
    }
}
