use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use ollama_router::cli::{inventory_lines, Cli, Commands};
use ollama_router::health::{reload_permanent_inventory, run as run_health};
use ollama_router::http::{build_upstream_client, make_app, AppState};
use ollama_router::provision::{ProvisionOrchestrator, ProvisionWatcher};
use ollama_router::warm::run as run_warm;
use ollama_router_core::cloud::{should_destroy_on_shutdown, DemandScale};
use ollama_router_core::fleet::{normalize_model, FleetState, Registry};
use ollama_router_core::jobs::{Job, JobStatus, PullOrchestrator};
use ollama_router_core::load_config;
use ollama_router_core::provision::{NodeProvisioner, ProvisionOpts, ProvisionStatus};
use ollama_router_core::routing::TargetSpec;
use ollama_router_verda::{VerdaClient, VerdaManager};

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

fn print_job(job: &Job) {
    println!(
        "{}",
        serde_json::json!({"job_id": job.id, "status": job.status.as_str()})
    );
}

fn spec_from_flags(all_nodes: bool, nodes: Option<&str>) -> anyhow::Result<TargetSpec> {
    if all_nodes {
        return Ok(TargetSpec::All);
    }
    if let Some(raw) = nodes {
        let parts: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        return TargetSpec::parse(Some(&parts)).map_err(|err| anyhow::anyhow!("{err}"));
    }
    Ok(TargetSpec::Placement)
}

async fn run_ensure(
    config: Option<PathBuf>,
    models: Vec<String>,
    all_nodes: bool,
    nodes: Option<String>,
    wait: bool,
) -> anyhow::Result<()> {
    let loaded = load_config(config.as_deref()).context("load config")?;
    let client = build_upstream_client(&loaded).context("http client")?;
    let orch = PullOrchestrator::new(Arc::new(loaded.clone()), client, None)?;
    let job = if models.is_empty() {
        let mut targets = BTreeMap::new();
        for node in &loaded.nodes {
            for model in loaded.tier_models_for_vram(node.static_capacity.vram_gb()) {
                targets
                    .entry(normalize_model(&model))
                    .or_insert_with(Vec::new)
                    .push(node.id.clone());
            }
        }
        if targets.is_empty() {
            eprintln!("error: pass --model or configure desired_model_tiers");
            std::process::exit(2);
        }
        orch.start_ensure_targets(targets)
            .map_err(|err| anyhow::anyhow!("{err}"))?
    } else {
        let spec = spec_from_flags(all_nodes, nodes.as_deref())?;
        orch.start_ensure(&models, spec, false, false)
            .map_err(|err| anyhow::anyhow!("{err}"))?
    };
    if !wait {
        print_job(&job);
    }
    let final_job = orch.wait_job(&job.id).await;
    if wait {
        print_job(&final_job);
        if final_job.status == JobStatus::Failed {
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn run_delete(
    config: Option<PathBuf>,
    models: Vec<String>,
    all_nodes: bool,
    nodes: Option<String>,
    wait: bool,
) -> anyhow::Result<()> {
    if models.is_empty() {
        eprintln!("error: pass --model (repeatable)");
        std::process::exit(2);
    }
    let loaded = load_config(config.as_deref()).context("load config")?;
    let client = build_upstream_client(&loaded).context("http client")?;
    let orch = PullOrchestrator::new(Arc::new(loaded), client, None)?;
    let spec = spec_from_flags(all_nodes, nodes.as_deref())?;
    let job = orch
        .start_delete(&models, spec, false)
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    if !wait {
        print_job(&job);
    }
    let final_job = orch.wait_job(&job.id).await;
    if wait {
        print_job(&final_job);
        if final_job.status == JobStatus::Failed {
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn serve(host: String, port: u16, config: Option<PathBuf>) -> anyhow::Result<()> {
    init_tracing();
    let loaded = load_config(config.as_deref()).context("load config")?;
    let mut state = AppState::from_config(loaded).context("build app state")?;
    let recovered = state.orchestrator.recover_incomplete_jobs().await;
    tracing::info!(
        recovered = recovered.len(),
        "recovered incomplete model jobs"
    );
    wire_verda(&mut state).context("verda")?;
    let app = make_app(state.clone());
    tokio::spawn(run_health(state.clone()));
    if state.config.policy.model_warm_enabled {
        tokio::spawn(run_warm(state.clone()));
    }
    if let Some(provisioner) = state.provisioner.clone() {
        let auto = state.config.provision_defaults.auto;
        let poll = state.config.provision_defaults.poll_interval_seconds;
        let watcher = ProvisionWatcher::new(state.registry.clone(), provisioner);
        tokio::spawn(watcher.run(auto, poll));
    }
    spawn_sighup_reloader(state.clone());
    let started_at = Instant::now();
    let destroy_on_shutdown = state.config.verda.destroy_on_shutdown;
    let shutdown_grace =
        Duration::from_secs_f64(state.config.verda.idle_grace_after_create_seconds.max(0.0));
    let verda_shutdown = state.verda.clone();
    if let Some(mgr) = state.verda.clone() {
        if state.config.verda.ensure_on_startup && !state.config.verda.auto_scale {
            let startup = mgr.clone();
            tokio::spawn(async move {
                if let Err(error) = startup.ensure(true).await {
                    tracing::error!(%error, "verda_ensure_on_startup_failed");
                }
            });
        }
        tokio::spawn(mgr.run_reconcile_loop());
    }
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(listen.addr = %addr, "ollama-router started");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server")?;
    if let Some(mgr) = verda_shutdown {
        let uptime = started_at.elapsed();
        if should_destroy_on_shutdown(destroy_on_shutdown, uptime, shutdown_grace) {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                "verda_destroy_on_shutdown"
            );
            let _ = mgr.destroy_all_owned().await;
        } else if destroy_on_shutdown {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                grace_seconds = shutdown_grace.as_secs_f64(),
                "verda_destroy_on_shutdown_skipped"
            );
        }
    }
    Ok(())
}

fn wire_verda(state: &mut AppState) -> anyhow::Result<()> {
    if !state.config.verda.enabled {
        return Ok(());
    }
    let client = VerdaClient::new(state.config.verda.clone()).context("verda client")?;
    let Some(provisioner) = state.provisioner.clone() else {
        anyhow::bail!("verda.enabled requires a provisioner");
    };
    let provisioner: Arc<dyn NodeProvisioner> = provisioner;
    let mgr = VerdaManager::new(
        state.config.clone(),
        client,
        state.registry.clone(),
        state.fleet_state.clone(),
        provisioner,
    );
    mgr.set_events(state.metrics.clone());
    state.demand = Arc::new(mgr.clone()) as Arc<dyn DemandScale>;
    state.verda = Some(mgr);
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    let _ = signal.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = ctrl_c => {}
            () = terminate => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

async fn run_provision(
    config: Option<PathBuf>,
    node: Option<String>,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<()> {
    let loaded = load_config(config.as_deref()).context("load config")?;
    let client = build_upstream_client(&loaded).context("http client")?;
    let config = Arc::new(loaded);
    let registry = Arc::new(Registry::new(&config));
    let fleet_state = Arc::new(FleetState::new(&config.state_path));
    let orch = ProvisionOrchestrator::new(config, client, Some(registry), Some(fleet_state));
    let ids: Option<Vec<String>> = node.map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let results = orch
        .provision_many(
            ids.as_deref(),
            ProvisionOpts {
                dry_run,
                force,
                wait_for_public_ssh: false,
            },
        )
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut failed = false;
    for result in &results {
        if result.status == ProvisionStatus::Fail {
            failed = true;
        }
        println!(
            "{}",
            serde_json::json!({
                "node_id": result.node_id.as_str(),
                "status": result.status.as_str(),
                "detail": result.detail,
                "tailscale_ip": result.tailscale_ip,
                "phase": result.phase,
            })
        );
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn run_nodes(config: Option<PathBuf>) -> anyhow::Result<()> {
    let loaded = load_config(config.as_deref()).context("load config")?;
    for line in inventory_lines(&loaded)? {
        println!("{line}");
    }
    Ok(())
}

async fn run_reload(config: Option<PathBuf>, host: String, port: u16) -> anyhow::Result<()> {
    let _ = config;
    let token = std::env::var("OLLAMA_ROUTER_ADMIN_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("set OLLAMA_ROUTER_ADMIN_TOKEN"))?;
    let url = format!("http://{host}:{port}/router/v1/reload");
    let client = reqwest::Client::builder().use_rustls_tls().build()?;
    let resp = client
        .post(&url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("reload failed: HTTP {status}");
    }
    println!("{}", serde_json::json!({"ok": true}));
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
        Commands::Ensure {
            config,
            models,
            all_nodes,
            nodes,
            wait,
        } => run_ensure(config, models, all_nodes, nodes, wait).await,
        Commands::Delete {
            config,
            models,
            all_nodes,
            nodes,
            wait,
        } => run_delete(config, models, all_nodes, nodes, wait).await,
        Commands::Nodes { config } => run_nodes(config),
        Commands::Reload { config, host, port } => run_reload(config, host, port).await,
        Commands::Provision {
            config,
            node,
            dry_run,
            force,
        } => run_provision(config, node, dry_run, force).await,
    }
}
