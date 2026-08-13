//! clap CLI: setup, serve, doctor, uninstall.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ollama-node-agent",
    version,
    about = "Install, supervise, and report capacity for a local Ollama node"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Elevated, idempotent converge (install Ollama + OS service).
    Setup {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Tailscale auth key env name (setup only; never written into serve).
        #[arg(long, env = "TS_AUTHKEY")]
        ts_authkey: Option<String>,
        /// Print the systemd unit (`agent_unit_text`) and exit. No filesystem writes.
        #[arg(long)]
        print_unit: bool,
    },
    /// Unprivileged HTTP on :11436.
    Serve {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, env = "OLLAMA_NODE_AGENT_HOST")]
        host: Option<String>,
        #[arg(long, env = "OLLAMA_NODE_AGENT_PORT")]
        port: Option<u16>,
        /// Windows SCM entry; ignored on other OS.
        #[arg(long, hide = true)]
        windows_service: bool,
    },
    /// Read-only: OS, GPU, Ollama, ports, Tailscale.
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Best-effort remove agent unit/plist/service (not Ollama unless --purge-ollama).
    Uninstall {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        purge_ollama: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_print_unit_parses() {
        let cli = Cli::try_parse_from(["ollama-node-agent", "setup", "--print-unit"]).unwrap();
        match cli.command {
            Commands::Setup { print_unit, .. } => assert!(print_unit),
            other => panic!("expected setup, got {other:?}"),
        }
    }
}
