use anyhow::Context;
use clap::Parser;
use ollama_node_agent::cli::{Cli, Commands};
use ollama_node_agent::config::AgentConfig;
use ollama_node_agent::setup::tunnel::{read_token_file, run_supervisor};
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
        } = &cli.command
        {
            return ollama_node_agent::windows_scm::run(
                ollama_node_agent::windows_scm::ServeOpts {
                    config: config.clone(),
                    host: host.clone(),
                    port: *port,
                },
            );
        }
        if let Commands::Tunnel {
            config,
            windows_service: true,
        } = &cli.command
        {
            return ollama_node_agent::windows_scm::run_tunnel(config.clone());
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
        Commands::Tunnel {
            config,
            windows_service: _,
        } => {
            ollama_node_agent::init_tracing();
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            let paths = SetupPaths::for_os();
            run_supervisor(cfg, paths).await
        }
        Commands::Setup {
            print_unit: true, ..
        } => {
            print!("{}", ollama_node_agent::setup::agent_unit_text());
            Ok(())
        }
        Commands::Setup {
            config,
            enable_token,
            enroll_url,
            enroll_token_env,
            print_unit: false,
        } => {
            ollama_node_agent::init_tracing();
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            let paths = SetupPaths::for_os();
            let file_token = cfg
                .tunnel
                .enable_token_file
                .as_deref()
                .and_then(|p| read_token_file(std::path::Path::new(p)).ok())
                .flatten();
            let key = enable_token.filter(|s| !s.trim().is_empty()).or(file_token);
            if key.is_some() && !cfg.tunnel.enable {
                tracing::info!("ZROK_ENABLE_TOKEN present but tunnel.enable is false; skip enable");
            }
            let ctx = SetupContext {
                config: &cfg,
                paths: &paths,
                enable_token: key.as_deref(),
                dry_commands: false,
            };
            let mut state = ollama_node_agent::setup::run(ctx).await?;
            if let Some(url) = enroll_url.filter(|s| !s.trim().is_empty()) {
                ollama_node_agent::setup::apply_enroll_flags(
                    &mut state,
                    &url,
                    enroll_token_env.as_deref(),
                )?;
                state.store(&paths.state)?;
            }
            tracing::info!(
                installed = state.ollama_installed,
                share_present = state.share_present(),
                "setup complete"
            );
            println!(
                "{}",
                ollama_node_agent::doctor::find_this_node_block(&state)
            );
            Ok(())
        }
        Commands::Doctor { config } => {
            let cfg = AgentConfig::load(config.as_deref()).context("load config")?;
            let report = ollama_node_agent::doctor::run(&cfg).await?;
            let paths = SetupPaths::for_os();
            let state = ollama_node_agent::setup::ConvergeState::load(&paths.state);
            println!(
                "{}",
                ollama_node_agent::doctor::find_this_node_block(&state)
            );
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
