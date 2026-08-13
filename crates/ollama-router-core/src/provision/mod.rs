//! SSH provision types, auth-key redaction, and the `NodeProvisioner` trait.
//!
//! The russh client lives in the binary crate. This module stays I/O-free
//! aside from reading a script path from disk for resolution helpers.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::config::{NodeConfig, NodeProvisionConfig, ProvisionDefaults, RouterConfig};
use crate::fleet::NodeId;

/// Remote script path on the target host.
pub const REMOTE_SCRIPT: &str = "/tmp/provision-ollama-gpu.sh";

/// Image / distro default when YAML `script_path` is unset.
pub const DEFAULT_SCRIPT_PATH: &str = "/usr/share/ollama-router/provision-ollama-gpu.sh";

/// Outcome of a provision attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionStatus {
    Ok,
    Skip,
    Fail,
    Dry,
}

impl ProvisionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skip => "skip",
            Self::Fail => "fail",
            Self::Dry => "dry",
        }
    }
}

/// Observable provision lifecycle phases (logs + metrics).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionPhase {
    WaitingPublicSsh,
    BootstrapTailscale,
    WaitingTailnetOpenssh,
    ProvisionOverTailscale,
    VerifyOllama,
    Ok,
    Fail,
    Cooldown,
    Skip,
    Dry,
}

impl ProvisionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingPublicSsh => "waiting_public_ssh",
            Self::BootstrapTailscale => "bootstrap_tailscale",
            Self::WaitingTailnetOpenssh => "waiting_tailnet_openssh",
            Self::ProvisionOverTailscale => "provision_over_tailscale",
            Self::VerifyOllama => "verify_ollama",
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Cooldown => "cooldown",
            Self::Skip => "skip",
            Self::Dry => "dry",
        }
    }
}

/// Per-call knobs for [`NodeProvisioner::provision_node`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ProvisionOpts {
    pub dry_run: bool,
    pub force: bool,
    /// Wait for public SSH (fresh Verda RUNNING only). Adopt/CLI/watcher stay false.
    pub wait_for_public_ssh: bool,
}

/// Result of provisioning one node. `detail` is allowlisted (no bodies/keys).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvisionResult {
    pub node_id: NodeId,
    pub status: ProvisionStatus,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

impl ProvisionResult {
    pub fn skip(node_id: NodeId, detail: impl Into<String>, phase: ProvisionPhase) -> Self {
        Self {
            node_id,
            status: ProvisionStatus::Skip,
            detail: detail.into(),
            tailscale_ip: None,
            phase: Some(phase.as_str().to_string()),
        }
    }

    pub fn fail(
        node_id: NodeId,
        detail: impl Into<String>,
        phase: ProvisionPhase,
        tailscale_ip: Option<String>,
    ) -> Self {
        Self {
            node_id,
            status: ProvisionStatus::Fail,
            detail: detail.into(),
            tailscale_ip,
            phase: Some(phase.as_str().to_string()),
        }
    }
}

/// Boxed future so [`NodeProvisioner`] is object-safe.
pub type ProvisionFuture<'a> = Pin<Box<dyn Future<Output = ProvisionResult> + Send + 'a>>;

/// Two-phase SSH provision (public bootstrap → Tailscale OpenSSH → verify).
pub trait NodeProvisioner: Send + Sync {
    fn provision_node(&self, node: NodeConfig, opts: ProvisionOpts) -> ProvisionFuture<'_>;
}

/// Never log a raw Tailscale auth key.
pub fn redact_authkey(key: Option<&str>) -> String {
    match key.map(str::trim).filter(|s| !s.is_empty()) {
        None => "(empty)".to_string(),
        Some(key) if key.starts_with("tskey-") => format!("tskey-*** (len={})", key.len()),
        Some(key) => format!("*** (len={})", key.len()),
    }
}

/// Cloud-built nodes inherit fleet-wide Tailscale knobs (including `ts_ephemeral`).
pub fn provision_config_from_defaults(defaults: &ProvisionDefaults) -> NodeProvisionConfig {
    NodeProvisionConfig {
        enabled: true,
        os_upgrade: true,
        skip_models: false,
        skip_ollama: false,
        ts_ephemeral: defaults.ts_ephemeral,
        ts_accept_routes: defaults.ts_accept_routes,
        ts_hostname: defaults.ts_hostname.clone(),
        ts_tags: defaults.ts_tags.clone(),
        ts_advertise_routes: defaults.ts_advertise_routes.clone(),
    }
}

impl NodeConfig {
    /// True when this node should be considered for SSH host provisioning.
    pub fn provision_enabled(&self) -> bool {
        if self.ssh.is_none() {
            return false;
        }
        self.provision.as_ref().map(|p| p.enabled).unwrap_or(true)
    }
}

/// Locate `provision-ollama-gpu.sh`.
pub fn resolve_provision_script(config: &RouterConfig) -> Result<PathBuf, String> {
    if let Some(path) = config
        .provision_defaults
        .script_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    if let Ok(env) = std::env::var("OLLAMA_PROVISION_SCRIPT") {
        let trimmed = env.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    let candidates = [
        PathBuf::from(DEFAULT_SCRIPT_PATH),
        PathBuf::from("scripts/provision-ollama-gpu.sh"),
        PathBuf::from("../scripts/provision-ollama-gpu.sh"),
        PathBuf::from("../../scripts/provision-ollama-gpu.sh"),
    ];
    for path in candidates {
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(
        "provision-ollama-gpu.sh not found; set provision_defaults.script_path or OLLAMA_PROVISION_SCRIPT"
            .into(),
    )
}

/// Read the resolved script bytes.
pub fn read_provision_script(config: &RouterConfig) -> Result<Vec<u8>, String> {
    let path = resolve_provision_script(config)?;
    std::fs::read(&path).map_err(|err| format!("script read failed: {err}"))
}

/// True when `path` looks like an existing file (tests).
pub fn script_exists(path: &Path) -> bool {
    path.is_file()
}

/// POSIX-safe single-quote for remote `env KEY=value`.
pub fn posix_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:@+=,".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_empty_and_tskey() {
        assert_eq!(redact_authkey(None), "(empty)");
        assert_eq!(redact_authkey(Some("")), "(empty)");
        assert_eq!(redact_authkey(Some("tskey-abc123")), "tskey-*** (len=12)");
        assert_eq!(redact_authkey(Some("other")), "*** (len=5)");
    }

    #[test]
    fn posix_quote_safe_and_unsafe() {
        assert_eq!(posix_quote("root"), "root");
        assert_eq!(posix_quote("a b"), "'a b'");
        assert_eq!(posix_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn provision_enabled_requires_ssh() {
        let mut node = NodeConfig {
            id: NodeId::parse("n").unwrap(),
            url: Some("http://127.0.0.1:11434".into()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: crate::config::Capacity::default(),
            max_inflight: None,
            ssh: None,
            provision: None,
        };
        assert!(!node.provision_enabled());
        node.ssh = Some(crate::config::NodeSshConfig {
            host: "10.0.0.1".into(),
            port: 22,
            user: "root".into(),
            key_file: Some("/run/secrets/ssh_key".into()),
            password_env: None,
        });
        assert!(node.provision_enabled());
        node.provision = Some(NodeProvisionConfig {
            enabled: false,
            ..NodeProvisionConfig::default()
        });
        assert!(!node.provision_enabled());
    }
}
