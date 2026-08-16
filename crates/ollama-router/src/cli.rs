//! clap CLI: serve, ensure, delete, nodes, reload.
//!
//! Thunder / RunPod subcommands are intentionally absent.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use ollama_router_core::config::RouterConfig;
use ollama_router_core::fleet::{FleetState, FleetStateEntry};

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
    /// Print inventory: origin, id, url, tunnel_backend, enroll_age (never share tokens).
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
}

/// Human inventory lines: `origin\tid\turl\ttunnel_backend\tenroll_age`.
///
/// Never prints share tokens. SUCCESS means both sources were read (empty
/// FleetState is still success).
pub fn inventory_lines(config: &RouterConfig) -> anyhow::Result<Vec<String>> {
    let now = unix_now();
    let state = FleetState::new(&config.state_path);
    let data = state.load()?;
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for node in &config.nodes {
        let id = node.id.as_str();
        seen.insert(id.to_string());
        let entry = data.get(id);
        let url = state
            .hydrate_url(&node.id)?
            .or_else(|| node.url.clone())
            .unwrap_or_else(|| "-".into());
        lines.push(format_inventory_line("permanent", id, &url, entry, now));
    }
    for (id, entry) in &data {
        if seen.contains(id) {
            continue;
        }
        let origin = match entry.managed_by.as_deref() {
            Some("verda") => "verda",
            Some("runpod") => "runpod",
            _ if inventory_state_row_visible(entry) => "adopt",
            _ => continue,
        };
        let url = display_state_url(entry);
        lines.push(format_inventory_line(origin, id, &url, Some(entry), now));
    }
    Ok(lines)
}

fn inventory_state_row_visible(entry: &FleetStateEntry) -> bool {
    matches!(entry.managed_by.as_deref(), Some("verda") | Some("runpod"))
        || entry
            .tunnel_backend
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
        || display_state_url(entry) != "-"
}

fn display_state_url(entry: &FleetStateEntry) -> String {
    entry
        .local_access_url
        .as_deref()
        .or(entry.url.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .to_string()
}

fn format_inventory_line(
    origin: &str,
    id: &str,
    url: &str,
    entry: Option<&FleetStateEntry>,
    now: f64,
) -> String {
    let backend = entry
        .and_then(|e| e.tunnel_backend.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("-");
    let age = enroll_age_label(entry.and_then(|e| e.updated_at), now, backend != "-");
    format!("{origin}\t{id}\t{url}\t{backend}\t{age}")
}

fn enroll_age_label(updated_at: Option<f64>, now: f64, enrolled: bool) -> String {
    if !enrolled {
        return "-".into();
    }
    let Some(at) = updated_at.filter(|t| *t > 0.0) else {
        return "-".into();
    };
    format!("{}", (now - at).max(0.0).round() as u64)
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ollama_router_core::config::{Capacity, NodeConfig};
    use ollama_router_core::fleet::{
        CloudInstanceId, EnrollPersist, NodeId, RunpodNodePersist, VerdaNodePersist,
    };

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    #[test]
    fn inventory_includes_fleet_and_verda_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("fleet-state.json");
        let store = FleetState::new(&state_path);
        let iid = CloudInstanceId::parse("abc").expect("iid");
        store
            .persist_verda_node(
                "verda-abc",
                VerdaNodePersist {
                    url: "http://127.0.0.1:41990",
                    instance_id: &iid,
                    location: "HEL",
                    instance_type: "gpu",
                    os_volume_id: None,
                    spot_price_per_hour: Some(0.4),
                    hostname: None,
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
            }],
            state_path,
            ..RouterConfig::default()
        };
        let lines = inventory_lines(&config).expect("lines");
        assert!(
            lines
                .iter()
                .any(|l| l == "permanent\tlocal\thttp://127.0.0.1:11434\t-\t-"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("verda\tverda-abc\thttp://127.0.0.1:41990\t-\t")),
            "{lines:?}"
        );
    }

    #[test]
    fn inventory_includes_runpod_origin_without_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("fleet-state.json");
        let store = FleetState::new(&state_path);
        let pod = CloudInstanceId::parse("pod1").expect("pod");
        store
            .persist_runpod_node(
                "runpod-pod1",
                RunpodNodePersist {
                    url: "http://127.0.0.1:41991",
                    pod_id: &pod,
                    gpu_type: "NVIDIA L4",
                    data_center: Some("US-CA-2"),
                    cost_per_hour: Some(0.39),
                    hostname: Some("or-rp-test-1"),
                },
            )
            .expect("persist");
        store
            .persist_enroll(
                "runpod-pod1",
                EnrollPersist {
                    url: "http://127.0.0.1:41991",
                    capacity_url: "http://127.0.0.1:41992",
                    ollama_share_id: "super-secret-runpod-share",
                    agent_share_id: "super-secret-runpod-agent",
                },
            )
            .expect("enroll");
        let config = RouterConfig {
            nodes: vec![],
            state_path,
            ..RouterConfig::default()
        };
        let lines = inventory_lines(&config).expect("lines");
        let joined = lines.join("\n");
        assert!(
            !joined.contains("secret"),
            "share tokens must not appear: {joined}"
        );
        let row = lines
            .iter()
            .find(|l| l.starts_with("runpod\trunpod-pod1\t"))
            .expect("runpod row");
        let cols: Vec<&str> = row.split('\t').collect();
        assert_eq!(cols.len(), 5, "{row}");
        assert_eq!(cols[0], "runpod");
        assert_eq!(cols[1], "runpod-pod1");
        assert_eq!(cols[2], "http://127.0.0.1:41991");
        assert_eq!(cols[3], "zrok");
        assert_ne!(cols[4], "-");
    }

    #[test]
    fn inventory_overlays_enroll_and_omits_share_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("fleet-state.json");
        let store = FleetState::new(&state_path);
        store
            .persist_enroll(
                "local",
                EnrollPersist {
                    url: "http://127.0.0.1:41990",
                    capacity_url: "http://127.0.0.1:41991",
                    ollama_share_id: "super-secret-share-token",
                    agent_share_id: "super-secret-agent-token",
                },
            )
            .expect("enroll");
        store
            .persist_enroll(
                "desk",
                EnrollPersist {
                    url: "http://127.0.0.1:42000",
                    capacity_url: "http://127.0.0.1:42001",
                    ollama_share_id: "desk-secret-share-token",
                    agent_share_id: "desk-secret-agent-token",
                },
            )
            .expect("adopt");
        let config = RouterConfig {
            nodes: vec![NodeConfig {
                id: nid("local"),
                url: Some("http://192.168.1.10:11434".into()),
                capacity_url: None,
                labels: Vec::new(),
                static_capacity: Capacity::default(),
                max_inflight: None,
            }],
            state_path,
            ..RouterConfig::default()
        };
        let lines = inventory_lines(&config).expect("lines");
        let joined = lines.join("\n");
        assert!(
            !joined.contains("secret"),
            "share tokens must not appear: {joined}"
        );
        let local = lines
            .iter()
            .find(|l| l.starts_with("permanent\tlocal\t"))
            .expect("local");
        let cols: Vec<&str> = local.split('\t').collect();
        assert_eq!(cols.len(), 5, "{local}");
        assert_eq!(cols[0], "permanent");
        assert_eq!(cols[1], "local");
        assert_eq!(cols[2], "http://127.0.0.1:41990");
        assert_eq!(cols[3], "zrok");
        assert_ne!(cols[4], "-");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("adopt\tdesk\thttp://127.0.0.1:42000\tzrok\t")),
            "{lines:?}"
        );
    }

    #[test]
    fn enroll_age_unset_without_tunnel() {
        assert_eq!(enroll_age_label(Some(1.0), 10.0, false), "-");
        assert_eq!(enroll_age_label(None, 10.0, true), "-");
        assert_eq!(enroll_age_label(Some(7.4), 10.0, true), "3");
    }
}
