//! macOS LaunchDaemon. Do not start a second `ollama serve` if the app is already up.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::{write_bytes_idempotent, write_token_file, ConvergeState, SetupContext};
use crate::collect::{ollama_tags_ok, ollama_version};

pub async fn converge(
    ctx: &SetupContext<'_>,
    ollama_bind: &str,
    agent_ip: IpAddr,
) -> anyhow::Result<ConvergeState> {
    let mut state = ConvergeState::load(&ctx.paths.state);
    state.schema = super::STATE_SCHEMA;
    state.bind = Some(ollama_bind.to_string());

    let version = ollama_version().await;
    if version.is_some() {
        tracing::info!("ollama already present (app or brew); skip pkg");
        state.ollama_installed = true;
        state.ollama_version = version;
    } else if ctx.dry_commands {
        tracing::info!("dry-run: skip brew/pkg install");
    } else {
        install_ollama_macos().await?;
        state.ollama_installed = ollama_version().await.is_some();
        state.ollama_version = ollama_version().await;
    }

    write_token_file(&ctx.paths.token_file, ctx.config.bearer_token())?;
    write_macos_env(
        ollama_bind,
        ctx.config.ollama.models_dir.as_deref(),
        &ctx.config.ollama.extra_env,
    )?;

    let plist = agent_plist(agent_ip, ctx.config.port);
    let plist_path = ctx.paths.unit_dir.join("com.ollama.node-agent.plist");
    write_bytes_idempotent(&plist_path, plist.as_bytes())?;
    state.unit_written = true;

    if !ctx.dry_commands {
        install_self_binary(&ctx.paths.bin_dir.join("ollama-node-agent")).await?;
        let running = ollama_tags_ok(&format!("http://{ollama_bind}")).await;
        if running {
            tracing::info!("ollama already serving; not launching a second process");
        } else {
            tracing::warn!(
                "ollama not listening on {ollama_bind}; start Ollama.app or `brew services` then re-run setup"
            );
        }
        let _ = Command::new("launchctl")
            .args(["bootout", "system/com.ollama.node-agent"])
            .status()
            .await;
        let status = Command::new("launchctl")
            .args(["bootstrap", "system", plist_path.to_str().unwrap_or("")])
            .status()
            .await?;
        if !status.success() {
            tracing::warn!("launchctl bootstrap failed; LaunchDaemon may need root");
        }
    }

    tracing::info!(
        "LaunchDaemon typically runs as root; UserName=ollama-node-agent is not created automatically"
    );
    state.last_converge = Some(now_rfc3339());
    state.store(&ctx.paths.state)?;
    Ok(state)
}

fn write_macos_env(
    bind: &str,
    models_dir: Option<&str>,
    extra: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let dir = Path::new("/Library/Application Support/Ollama");
    let _ = std::fs::create_dir_all(dir);
    let env_path = dir.join("env");
    let mut body = format!("OLLAMA_HOST={bind}\n");
    if let Some(models) = models_dir.filter(|d| !d.is_empty()) {
        body.push_str(&format!("OLLAMA_MODELS={models}\n"));
    }
    for (k, v) in extra {
        if k.eq_ignore_ascii_case("OLLAMA_HOST") || k.eq_ignore_ascii_case("OLLAMA_MODELS") {
            continue;
        }
        body.push_str(&format!("{k}={v}\n"));
    }
    let _ = std::fs::write(env_path, body);
    Ok(())
}

fn agent_plist(agent_ip: IpAddr, port: u16) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.ollama.node-agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/ollama-node-agent</string>
    <string>serve</string>
    <string>--config</string>
    <string>/Library/Application Support/ollama-node-agent/config.yaml</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>OLLAMA_NODE_AGENT_HOST</key>
    <string>{agent_ip}</string>
    <key>OLLAMA_NODE_AGENT_PORT</key>
    <string>{port}</string>
  </dict>
</dict>
</plist>
"#
    )
}

async fn install_ollama_macos() -> anyhow::Result<()> {
    if Command::new("brew")
        .arg("--version")
        .status()
        .await
        .is_ok_and(|s| s.success())
    {
        let status = Command::new("brew")
            .args(["install", "ollama"])
            .status()
            .await?;
        if status.success() {
            return Ok(());
        }
    }
    anyhow::bail!("install Ollama.app or `brew install ollama`, then re-run setup")
}

async fn install_self_binary(dest: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&exe, dest)?;
    Ok(())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn plist_contains_label() {
        let p = agent_plist(IpAddr::V4(Ipv4Addr::LOCALHOST), 11436);
        assert!(p.contains("com.ollama.node-agent"));
        assert!(p.contains("11436"));
    }
}
