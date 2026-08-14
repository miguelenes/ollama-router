//! Read-only inventory for operators.

use serde::Serialize;

use crate::collect::{ollama_tags_ok, ollama_version};
use crate::config::AgentConfig;
use crate::listen::{format_host_port, resolve_bind, AddrSource, HostAddrs};
use crate::redact::share_token_id;
use crate::setup::tunnel::{file_url, find_path, zrok_bin_present};
use crate::setup::{ConvergeState, SetupPaths};

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
    pub tunnel_ok: bool,
    pub share_present: bool,
    pub ollama_loopback_ok: bool,
    /// Redacted share token id (prefix), never the full unique-name.
    pub ollama_share_id: Option<String>,
    /// Redacted share token id (prefix), never the full unique-name.
    pub agent_share_id: Option<String>,
    /// `pending` until `--enroll-url` is stored; `configured` after.
    pub enroll: String,
    pub enroll_url: Option<String>,
    pub find_url: Option<String>,
    pub notes: Vec<String>,
}

/// Operator stdout block for `setup` and `doctor`. Never prints full share tokens.
pub fn find_this_node_block(state: &ConvergeState) -> String {
    format_find_this_node(
        state.ollama_share_token.as_deref(),
        state.agent_share_token.as_deref(),
        state.enroll_url.as_deref(),
    )
}

pub fn format_find_this_node(
    ollama_share: Option<&str>,
    agent_share: Option<&str>,
    enroll_url: Option<&str>,
) -> String {
    let ollama = redacted_share(ollama_share);
    let agent = redacted_share(agent_share);
    let enroll_url = enroll_url.map(str::trim).filter(|s| !s.is_empty());
    let enroll = if enroll_url.is_some() {
        "configured"
    } else {
        "pending"
    };
    let mut lines = vec![
        "find this node".to_string(),
        format!("  ollama_share_id: {ollama}"),
        format!("  agent_share_id: {agent}"),
        format!("  enroll: {enroll}"),
    ];
    if let Some(url) = enroll_url {
        lines.push(format!("  enroll_url: {url}"));
    }
    lines.join("\n")
}

fn redacted_share(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(share_token_id)
        .unwrap_or_else(|| "-".into())
}

fn enroll_status(enroll_url: Option<&str>) -> String {
    if enroll_url.map(str::trim).is_some_and(|s| !s.is_empty()) {
        "configured".into()
    } else {
        "pending".into()
    }
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
    let ollama_loopback_ok = ollama_tags_ok("http://127.0.0.1:11434").await;
    let gpu_backend = match crate::collect::collect_live(config, &ollama_listen, None).await {
        Ok(snap) => snap.status.gpu_backend.as_str().into(),
        Err(err) => {
            notes.push(format!("collect failed: {err}"));
            "unknown".into()
        }
    };
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
    #[cfg(target_os = "linux")]
    {
        if let Some(note) = linux_no_systemd_note(crate::setup::systemd_detected()) {
            notes.push(note.into());
        }
    }
    let paths = SetupPaths::for_os();
    let state = ConvergeState::load(&paths.state);
    let share_present = state.share_present();
    let zrok_ok = zrok_bin_present(&config.tunnel.zrok_bin);
    let tunnel_ok = if config.tunnel.enable {
        zrok_ok && share_present
    } else {
        true
    };
    if config.tunnel.enable && !zrok_ok {
        notes.push(format!(
            "zrok binary not found ({}); install zrok or set tunnel.zrok_bin",
            config.tunnel.zrok_bin
        ));
    }
    if !config.tunnel.enable {
        notes.push("tunnel.enable is false (LAN / local-dev)".into());
    }
    let ollama_share_id = state
        .ollama_share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(share_token_id);
    let agent_share_id = state
        .agent_share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(share_token_id);
    let enroll_url = state
        .enroll_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let enroll = enroll_status(enroll_url.as_deref());
    let find_url = share_present.then(|| file_url(&find_path(&paths)));
    Ok(DoctorReport {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        gpu_backend,
        ollama_installed: version.is_some(),
        ollama_running: running,
        ollama_version: version,
        ollama_listen,
        agent_listen,
        tunnel_ok,
        share_present,
        ollama_loopback_ok,
        ollama_share_id,
        agent_share_id,
        enroll,
        enroll_url,
        find_url,
        notes,
    })
}

pub const NO_SYSTEMD_NOTE: &str =
    "no systemd; start `ollama-node-agent serve` under your supervisor";

pub(crate) fn linux_no_systemd_note(systemd: bool) -> Option<&'static str> {
    (!systemd).then_some(NO_SYSTEMD_NOTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::ConvergeState;

    #[test]
    fn no_systemd_note_is_stable() {
        assert_eq!(
            NO_SYSTEMD_NOTE,
            "no systemd; start `ollama-node-agent serve` under your supervisor"
        );
        assert_eq!(linux_no_systemd_note(true), None);
        assert_eq!(linux_no_systemd_note(false), Some(NO_SYSTEMD_NOTE));
    }

    #[test]
    fn find_block_redacts_share_and_shows_enroll() {
        let pending =
            format_find_this_node(Some("abcdefghij-secret"), Some("klmnopqr-secret"), None);
        assert!(pending.starts_with("find this node\n"), "{pending}");
        assert!(pending.contains("ollama_share_id: abcdefgh…"), "{pending}");
        assert!(pending.contains("agent_share_id: klmnopqr…"), "{pending}");
        assert!(pending.contains("enroll: pending"), "{pending}");
        assert!(!pending.contains("secret"), "{pending}");
        assert!(!pending.contains("enroll_url:"), "{pending}");

        let state = ConvergeState {
            ollama_share_token: Some("share-ollama-token".into()),
            agent_share_token: Some("share-agent-token".into()),
            enroll_url: Some("http://router:11435/router/v1/nodes/enroll".into()),
            ..ConvergeState::default()
        };
        let configured = find_this_node_block(&state);
        assert!(configured.contains("enroll: configured"), "{configured}");
        assert!(
            configured.contains("enroll_url: http://router:11435/router/v1/nodes/enroll"),
            "{configured}"
        );
        assert!(!configured.contains("share-ollama-token"), "{configured}");
    }
}
