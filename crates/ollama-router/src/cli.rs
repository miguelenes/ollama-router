//! clap CLI: serve, ensure, delete, nodes, reload, provision.
//!
//! Thunder / RunPod subcommands are intentionally absent.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ollama_router_core::config::RouterConfig;
use ollama_router_core::fleet::FleetState;

#[derive(Debug, Parser)]
#[command(
    name = "ollama-router",
    version,
    about = "Ollama-compatible fleet proxy"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run the router HTTP server.
    Serve {
        /// Optional tunables YAML overlay (`OLLAMA_ROUTER_CONFIG`).
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, env = "OLLAMA_ROUTER_HOST", default_value = "0.0.0.0")]
        host: String,
        #[arg(long, env = "OLLAMA_ROUTER_PORT", default_value_t = 11434)]
        port: u16,
    },
    /// Idempotently pull models onto placement-eligible nodes.
    Ensure {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Model name (repeatable).
        #[arg(long = "model", action = clap::ArgAction::Append)]
        models: Vec<String>,
        /// Bypass placement and target every configured node.
        #[arg(long)]
        all_nodes: bool,
        /// Comma-separated node ids.
        #[arg(long)]
        nodes: Option<String>,
        /// Wait for the job to finish.
        #[arg(long)]
        wait: bool,
    },
    /// Remove models from fleet nodes (default: current holders).
    Delete {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long = "model", action = clap::ArgAction::Append)]
        models: Vec<String>,
        #[arg(long)]
        all_nodes: bool,
        #[arg(long)]
        nodes: Option<String>,
        #[arg(long)]
        wait: bool,
    },
    /// Print live node inventory (fleet.yaml + FleetState Verda rows).
    Nodes {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// POST /router/v1/reload (same as SIGHUP). Requires OLLAMA_ROUTER_ADMIN_TOKEN.
    Reload {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, env = "OLLAMA_ROUTER_HOST", default_value = "127.0.0.1")]
        host: String,
        #[arg(long, env = "OLLAMA_ROUTER_PORT", default_value_t = 11434)]
        port: u16,
    },
    /// SSH-provision GPU hosts from fleet.yaml ssh blocks.
    Provision {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Comma-separated node ids (default: all provisionable).
        #[arg(long)]
        node: Option<String>,
        /// Probe SSH only; print planned actions.
        #[arg(long)]
        dry_run: bool,
        /// Ignore cooldown.
        #[arg(long)]
        force: bool,
    },
}

/// Human inventory lines: `origin\tid\turl`. SUCCESS means both sources were read
/// (empty Verda set is still success).
pub fn inventory_lines(config: &RouterConfig) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    for node in &config.nodes {
        let url = node.url.as_deref().unwrap_or("-");
        lines.push(format!("permanent\t{}\t{url}", node.id.as_str()));
    }
    let state = FleetState::new(&config.state_path);
    for (id, entry) in state.list_verda_nodes()? {
        let url = entry
            .url
            .as_deref()
            .or(entry.tailscale_ip.as_deref())
            .unwrap_or("-");
        lines.push(format!("verda\t{id}\t{url}"));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ollama_router_core::config::{Capacity, NodeConfig};
    use ollama_router_core::fleet::{NodeId, VerdaInstanceId, VerdaNodePersist};

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    #[test]
    fn inventory_includes_fleet_and_verda_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("fleet-state.json");
        let store = FleetState::new(&state_path);
        let iid = VerdaInstanceId::parse("abc").expect("iid");
        store
            .persist_verda_node(
                "verda-abc",
                VerdaNodePersist {
                    url: "http://100.64.0.2:11434",
                    instance_id: &iid,
                    location: "HEL",
                    instance_type: "gpu",
                    os_volume_id: None,
                    tailscale_ip: Some("100.64.0.2"),
                    spot_price_per_hour: Some(0.4),
                },
            )
            .expect("persist");
        let config = RouterConfig {
            nodes: vec![NodeConfig {
                id: nid("local"),
                url: Some("http://127.0.0.1:11434".into()),
                capacity_url: None,
                labels: Vec::new(),
                static_capacity: Capacity::default(),
                max_inflight: None,
                ssh: None,
                provision: None,
            }],
            state_path,
            ..RouterConfig::default()
        };
        let lines = inventory_lines(&config).expect("lines");
        assert!(
            lines.iter().any(|l| l.starts_with("permanent\tlocal\t")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("verda\tverda-abc\thttp://100.64.0.2:11434")),
            "{lines:?}"
        );
    }
}
