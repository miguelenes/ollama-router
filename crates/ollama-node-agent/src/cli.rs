//! clap CLI: setup, serve, doctor, uninstall, tunnel.

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
    /// Elevated, idempotent converge (install Ollama + OS service + zrok sidecar).
    Setup {
        #[arg(long)]
        config: Option<PathBuf>,
        /// zrok enable token (setup only; never written into serve). Never log the value.
        #[arg(long, env = "ZROK_ENABLE_TOKEN")]
        enable_token: Option<String>,
        /// Router enroll URL. Written to state.json only — never the admin bearer.
        #[arg(long)]
        enroll_url: Option<String>,
        /// Env var *name* holding `OLLAMA_ROUTER_ADMIN_TOKEN` (or equivalent). Not the token.
        #[arg(long)]
        enroll_token_env: Option<String>,
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
    /// Supervised zrok private shares (dedicated unit `ollama-node-agent-tunnel`).
    Tunnel {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Windows SCM entry; ignored on other OS.
        #[arg(long, hide = true)]
        windows_service: bool,
    },
    /// Read-only: OS, GPU, Ollama, ports, tunnel. Prints a find-this-node block then JSON.
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

    #[test]
    fn setup_enable_token_parses() {
        let cli = Cli::try_parse_from(["ollama-node-agent", "setup", "--enable-token", "secret"])
            .unwrap();
        match cli.command {
            Commands::Setup { enable_token, .. } => {
                assert_eq!(enable_token.as_deref(), Some("secret"));
            }
            other => panic!("expected setup, got {other:?}"),
        }
    }

    #[test]
    fn setup_enroll_flags_parse() {
        let cli = Cli::try_parse_from([
            "ollama-node-agent",
            "setup",
            "--enroll-url",
            "http://router:11435/router/v1/nodes/enroll",
            "--enroll-token-env",
            "OLLAMA_ROUTER_ADMIN_TOKEN",
        ])
        .unwrap();
        match cli.command {
            Commands::Setup {
                enroll_url,
                enroll_token_env,
                ..
            } => {
                assert_eq!(
                    enroll_url.as_deref(),
                    Some("http://router:11435/router/v1/nodes/enroll")
                );
                assert_eq!(
                    enroll_token_env.as_deref(),
                    Some("OLLAMA_ROUTER_ADMIN_TOKEN")
                );
            }
            other => panic!("expected setup, got {other:?}"),
        }
    }

    #[test]
    fn tunnel_parses() {
        let cli = Cli::try_parse_from(["ollama-node-agent", "tunnel"]).unwrap();
        assert!(matches!(cli.command, Commands::Tunnel { .. }));
    }
}
