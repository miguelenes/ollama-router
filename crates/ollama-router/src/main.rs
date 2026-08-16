use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use ollama_router::bootstrap::run as run_bootstrap;
use ollama_router::cli::{inventory_lines, Cli, Commands};
use ollama_router::health::{reload_permanent_inventory, run as run_health};
use ollama_router::http::{build_upstream_client, make_app, AppState};
use ollama_router::warm::run as run_warm;
use ollama_router_core::cloud::{
    should_destroy_on_shutdown, CloudProviderHandle, MultiProviderDemand,
};
use ollama_router_core::fleet::normalize_model;
use ollama_router_core::jobs::{Job, JobStatus, PullOrchestrator};
use ollama_router_core::load_config;
use ollama_router_core::routing::TargetSpec;
use ollama_router_runpod::{RunpodClient, RunpodManager};
use ollama_router_verda::{VerdaClient, VerdaManager};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const SUPERVISOR_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

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
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?
    } else {
        let spec = spec_from_flags(all_nodes, nodes.as_deref())?;
        orch.start_ensure(&models, spec, false, false)
            .await
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
        .await
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
    let shutdown = CancellationToken::new();
    let loaded = load_config(config.as_deref()).context("load config")?;
    let mut state =
        AppState::from_config_with_shutdown(loaded, shutdown.clone()).context("build app state")?;
    if let Err(error) = state
        .tunnels
        .restore_fleet(&state.fleet_state, &state.registry)
        .await
    {
        tracing::warn!(%error, "enroll tunnel restore failed");
    }
    let recovered = state.orchestrator.recover_incomplete_jobs().await;
    tracing::info!(
        recovered = recovered.len(),
        "recovered incomplete model jobs"
    );
    wire_cloud_providers(&mut state, shutdown.clone()).context("cloud providers")?;
    let app = make_app(state.clone());
    let mut supervisor = Supervisor::new();
    supervisor.spawn(run_health(state.clone(), shutdown.clone()));
    if state.config.policy.model_warm_enabled {
        supervisor.spawn(run_warm(state.clone(), shutdown.clone()));
    }
    if state.config.bootstrap_desired_models {
        supervisor.spawn(run_bootstrap(state.clone(), shutdown.clone()));
    }
    #[cfg(unix)]
    supervisor.spawn(run_sighup_reloader(state.clone(), shutdown.clone()));
    let started_at = Instant::now();
    let verda_destroy_on_shutdown = state.config.verda.destroy_on_shutdown;
    let verda_shutdown_grace =
        Duration::from_secs_f64(state.config.verda.idle_grace_after_create_seconds.max(0.0));
    let runpod_destroy_on_shutdown = state.config.runpod.destroy_on_shutdown;
    let runpod_shutdown_grace =
        Duration::from_secs_f64(state.config.runpod.idle_grace_after_create_seconds.max(0.0));
    let verda_shutdown = state.verda.clone();
    let runpod_shutdown = state.runpod.clone();
    if let Some(mgr) = state.verda.clone() {
        if state.config.verda.ensure_on_startup && !state.config.verda.auto_scale {
            let startup = mgr.clone();
            let token = shutdown.clone();
            supervisor.spawn(async move {
                tokio::select! {
                    biased;
                    () = token.cancelled() => {}
                    result = startup.ensure(true) => {
                        if let Err(error) = result {
                            tracing::error!(%error, "verda_ensure_on_startup_failed");
                        }
                    }
                }
            });
        }
        supervisor.spawn(mgr.run_reconcile_loop());
    }
    if let Some(mgr) = state.runpod.clone() {
        supervisor.spawn(mgr.run_reconcile_loop());
    }
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(listen.addr = %addr, "ollama-router started");
    let serve_shutdown = shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            serve_shutdown.cancel();
        })
        .await
        .context("server")?;
    tracing::info!("supervisor_shutdown");
    shutdown.cancel();
    supervisor.join_or_abort(SUPERVISOR_JOIN_TIMEOUT).await;
    if let Some(mgr) = verda_shutdown {
        mgr.abort_demand().await;
        let uptime = started_at.elapsed();
        if should_destroy_on_shutdown(verda_destroy_on_shutdown, uptime, verda_shutdown_grace) {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                "verda_destroy_on_shutdown"
            );
            let _ = mgr.destroy_all_owned().await;
        } else if verda_destroy_on_shutdown {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                grace_seconds = verda_shutdown_grace.as_secs_f64(),
                "verda_destroy_on_shutdown_skipped"
            );
        }
    }
    if let Some(mgr) = runpod_shutdown {
        mgr.abort_demand().await;
        let uptime = started_at.elapsed();
        if should_destroy_on_shutdown(runpod_destroy_on_shutdown, uptime, runpod_shutdown_grace) {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                "runpod_destroy_on_shutdown"
            );
            let _ = mgr.destroy_all_owned().await;
        } else if runpod_destroy_on_shutdown {
            tracing::info!(
                uptime_seconds = uptime.as_secs_f64(),
                grace_seconds = runpod_shutdown_grace.as_secs_f64(),
                "runpod_destroy_on_shutdown_skipped"
            );
        }
    }
    Ok(())
}

fn wire_cloud_providers(state: &mut AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    let mut handles: Vec<Arc<dyn CloudProviderHandle>> = Vec::new();

    if state.config.verda.enabled {
        let client = VerdaClient::new(state.config.verda.clone()).context("verda client")?;
        let mgr = VerdaManager::with_shutdown(
            state.config.clone(),
            client,
            state.registry.clone(),
            state.fleet_state.clone(),
            shutdown.clone(),
        );
        mgr.set_events(state.metrics.clone());
        handles.push(Arc::new(mgr.clone()) as Arc<dyn CloudProviderHandle>);
        state.verda = Some(mgr);
    }

    if state.config.runpod.enabled {
        let client = RunpodClient::new(state.config.runpod.clone()).context("runpod client")?;
        let mgr = RunpodManager::with_shutdown(
            state.config.clone(),
            client,
            state.registry.clone(),
            state.fleet_state.clone(),
            shutdown,
        );
        mgr.set_events(state.metrics.clone());
        handles.push(Arc::new(mgr.clone()) as Arc<dyn CloudProviderHandle>);
        state.runpod = Some(mgr);
    }

    if handles.is_empty() {
        // AppState defaults to NoopDemandScale; leave it.
        return Ok(());
    }
    state.demand = Arc::new(MultiProviderDemand::new(handles));
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

#[cfg(unix)]
async fn run_sighup_reloader(state: AppState, shutdown: CancellationToken) {
    let mut hangup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::error!(%error, "failed to register SIGHUP handler");
            return;
        }
    };
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            recv = hangup.recv() => {
                let Some(()) = recv else {
                    return;
                };
                if let Err(error) = reload_permanent_inventory(&state).await {
                    tracing::error!(%error, "SIGHUP fleet reload failed");
                } else if state.config.bootstrap_desired_models {
                    let boot = state.clone();
                    let token = shutdown.clone();
                    tokio::spawn(async move {
                        run_bootstrap(boot, token).await;
                    });
                }
            }
        }
    }
}

struct Supervisor {
    tasks: JoinSet<()>,
}

impl Supervisor {
    fn new() -> Self {
        Self {
            tasks: JoinSet::new(),
        }
    }

    fn spawn<F>(&mut self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let _abort = self.tasks.spawn(fut);
    }

    async fn join_or_abort(mut self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.tasks.join_next()).await {
                Ok(Some(Ok(()))) => {}
                Ok(Some(Err(err))) => {
                    if err.is_panic() {
                        tracing::error!("supervisor_task_panic");
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    tracing::warn!(
                        timeout_seconds = timeout.as_secs_f64(),
                        "supervisor_join_timeout"
                    );
                    self.tasks.abort_all();
                    while self.tasks.join_next().await.is_some() {}
                    return;
                }
            }
        }
    }
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
    }
}
