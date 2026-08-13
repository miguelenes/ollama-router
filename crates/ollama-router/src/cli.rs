//! clap CLI: serve, ensure, delete, nodes, provision.
//!
//! Thunder / RunPod subcommands are intentionally absent.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    /// Print live node inventory.
    Nodes {
        #[arg(long)]
        config: Option<PathBuf>,
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
