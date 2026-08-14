//! Windows SCM entry for `serve --windows-service`.
//!
//! `define_windows_service!` expands to an `extern "system"` FFI thunk
//! (`fn(u32, *mut *mut u16)`). That is the only unsafe in this crate; do not
//! inherit workspace `unsafe_code = "forbid"` (`lints.workspace = true`).
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::service_identity::{SERVICE_NAME, TUNNEL_SERVICE_NAME};

windows_service::define_windows_service!(ffi_service_main, service_main);
windows_service::define_windows_service!(ffi_tunnel_main, tunnel_service_main);

#[derive(Debug)]
pub struct ServeOpts {
    pub config: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

static SERVE_OPTS: OnceLock<ServeOpts> = OnceLock::new();
static TUNNEL_CONFIG: OnceLock<Option<PathBuf>> = OnceLock::new();

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// Block on the SCM dispatcher until the service stops.
pub fn run(opts: ServeOpts) -> anyhow::Result<()> {
    crate::init_tracing();
    SERVE_OPTS
        .set(opts)
        .map_err(|_| anyhow::anyhow!("Windows service options already set"))?;
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|error| anyhow::anyhow!("service dispatcher: {error}"))
}

/// Block on the SCM dispatcher for the zrok sidecar service.
pub fn run_tunnel(config: Option<PathBuf>) -> anyhow::Result<()> {
    crate::init_tracing();
    TUNNEL_CONFIG
        .set(config)
        .map_err(|_| anyhow::anyhow!("Windows tunnel options already set"))?;
    service_dispatcher::start(TUNNEL_SERVICE_NAME, ffi_tunnel_main)
        .map_err(|error| anyhow::anyhow!("tunnel service dispatcher: {error}"))
}

fn service_main(_args: Vec<OsString>) {
    if let Err(error) = run_service() {
        tracing::error!(%error, "Windows service failed");
    }
}

fn run_service() -> anyhow::Result<()> {
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                let _ = shutdown_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
        .map_err(|error| anyhow::anyhow!("register service control handler: {error}"))?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|error| anyhow::anyhow!("set running status: {error}"))?;

    let opts = SERVE_OPTS
        .get()
        .context("Windows service options missing")?;
    let (cfg, bind, ollama_listen) =
        crate::http::prepare_serve(opts.config.as_deref(), opts.host.clone(), opts.port)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let serve_result = rt.block_on(async move {
        crate::http::serve_with_shutdown(cfg, bind, ollama_listen, async move {
            let _ = shutdown_rx.wait_for(|stop| *stop).await;
        })
        .await
    });

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
    serve_result
}

fn tunnel_service_main(_args: Vec<OsString>) {
    if let Err(error) = run_tunnel_service() {
        tracing::error!(%error, "Windows tunnel service failed");
    }
}

fn run_tunnel_service() -> anyhow::Result<()> {
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                let _ = shutdown_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let status_handle = service_control_handler::register(TUNNEL_SERVICE_NAME, event_handler)
        .map_err(|error| anyhow::anyhow!("register tunnel service control handler: {error}"))?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|error| anyhow::anyhow!("set tunnel running status: {error}"))?;

    let config = TUNNEL_CONFIG
        .get()
        .context("Windows tunnel options missing")?;
    let cfg = crate::config::AgentConfig::load(config.as_deref()).context("load config")?;
    let paths = crate::setup::SetupPaths::for_os();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let result = rt.block_on(async move {
        tokio::select! {
            result = crate::setup::tunnel::run_supervisor(cfg, paths) => result,
            () = async {
                let _ = shutdown_rx.wait_for(|stop| *stop).await;
            } => Ok(()),
        }
    });

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });
    result
}
