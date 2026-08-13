use clap::Parser;
use ollama_router::cli::{Cli, Commands};

#[test]
fn parses_serve() {
    let cli = Cli::try_parse_from(["ollama-router", "serve"]).expect("parse serve");
    match cli.command {
        Commands::Serve { host, port, .. } => {
            assert_eq!(host, "0.0.0.0");
            assert_eq!(port, 11434);
        }
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn parses_ensure() {
    let cli = Cli::try_parse_from(["ollama-router", "ensure", "--model", "qwen3-embedding:8b"])
        .expect("parse ensure");
    assert!(matches!(cli.command, Commands::Ensure { .. }));
}

#[test]
fn parses_delete() {
    let cli = Cli::try_parse_from(["ollama-router", "delete", "--model", "unused"])
        .expect("parse delete");
    assert!(matches!(cli.command, Commands::Delete { .. }));
}

#[test]
fn parses_nodes() {
    let cli = Cli::try_parse_from(["ollama-router", "nodes"]).expect("parse nodes");
    assert!(matches!(cli.command, Commands::Nodes { .. }));
}

#[test]
fn parses_provision() {
    let cli =
        Cli::try_parse_from(["ollama-router", "provision", "--dry-run"]).expect("parse provision");
    assert!(matches!(
        cli.command,
        Commands::Provision { dry_run: true, .. }
    ));
}
