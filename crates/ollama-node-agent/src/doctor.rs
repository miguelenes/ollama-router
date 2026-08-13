//! Read-only inventory for operators.

use serde::Serialize;

use crate::collect::{ollama_tags_ok, ollama_version};
use crate::config::AgentConfig;
use crate::listen::{format_host_port, resolve_bind, AddrSource, HostAddrs};

#[derive(Serialize)]
pub struct DoctorReport {
    pub os: String,
    pub arch: String,
    pub gpu_backend: String,
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub ollama_version: Option<String>,
    pub ollama_listen: String,
    pub agent_listen: String,
    pub tailscale_ok: bool,
    pub notes: Vec<String>,
}

pub async fn run(config: &AgentConfig) -> anyhow::Result<DoctorReport> {
    let token_set = config.bearer_token().is_some();
    let addrs = HostAddrs;
    let addrs: &dyn AddrSource = &addrs;
    let mut notes = Vec::new();
    let ollama_ip = match resolve_bind(&config.ollama.listen, addrs, token_set) {
        Ok(ip) => ip,
        Err(err) => {
            notes.push(err.to_string());
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        }
    };
    let agent_ip = match resolve_bind(&config.listen, addrs, token_set) {
        Ok(ip) => ip,
        Err(err) => {
            notes.push(err.to_string());
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        }
    };
    let ollama_listen = format_host_port(ollama_ip, 11434);
    let agent_listen = format_host_port(agent_ip, config.port);
    let version = ollama_version().await;
    let running = ollama_tags_ok(&format!("http://{ollama_listen}")).await;
    let snap = crate::collect::collect_live(config, &ollama_listen).await;
    if cfg!(windows) {
        notes.push(
            "Windows: do not also run the tray app on :11434; NVIDIA services usually need LocalSystem"
                .into(),
        );
    }
    if cfg!(target_os = "macos") {
        notes.push(
            "macOS LaunchDaemon often runs as root; GPU backend is Metal (no nvidia-smi)".into(),
        );
    }
    let ts = addrs_have_tailscale();
    Ok(DoctorReport {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        gpu_backend: snap.status.gpu_backend.as_str().into(),
        ollama_installed: version.is_some(),
        ollama_running: running,
        ollama_version: version,
        ollama_listen,
        agent_listen,
        tailscale_ok: ts,
        notes,
    })
}

fn addrs_have_tailscale() -> bool {
    let addrs = HostAddrs;
    let addrs: &dyn AddrSource = &addrs;
    addrs
        .ipv4s()
        .into_iter()
        .any(crate::listen::is_tailscale_ipv4)
}
