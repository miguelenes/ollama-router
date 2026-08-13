//! Windows: silent Setup.exe (desktop) or standalone zip (headless service).
//! GPU NVIDIA under a service often needs LocalSystem; CPU-only can be LocalService.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::{write_bytes_idempotent, write_token_file, ConvergeState, SetupContext};
use crate::collect::ollama_version;

/// Inno Setup flags from ollama/ollama `scripts/install.ps1`.
pub const SETUP_SILENT_ARGS: &[&str] = &["/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES"];

const SETUP_URL: &str = "https://ollama.com/download/OllamaSetup.exe";
const ZIP_URL: &str = "https://github.com/ollama/ollama/releases/latest/download/ollama-windows-amd64.zip";

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
        tracing::info!("ollama already installed; skip download");
        state.ollama_installed = true;
        state.ollama_version = version;
    } else if ctx.dry_commands {
        tracing::info!("dry-run: skip OllamaSetup.exe / zip");
    } else {
        install_ollama_windows().await?;
        state.ollama_installed = ollama_version().await.is_some();
        state.ollama_version = ollama_version().await;
    }

    write_token_file(&ctx.paths.token_file, ctx.config.bearer_token())?;
    let env_body = format!("OLLAMA_HOST={ollama_bind}\n");
    write_bytes_idempotent(&ctx.paths.unit_dir.join("ollama.env"), env_body.as_bytes())?;

    tracing::info!(
        agent_ip = %agent_ip,
        "Windows: do not also run the tray app on :11434; NVIDIA in a service usually needs LocalSystem"
    );

    if !ctx.dry_commands {
        install_self_binary(&ctx.paths.bin_dir.join("ollama-node-agent.exe")).await?;
        register_scheduled_task(&ctx.paths.bin_dir.join("ollama-node-agent.exe")).await?;
        let _ = set_firewall(ollama_bind).await;
    }

    state.unit_written = true;
    state.last_converge = Some(now_rfc3339());
    state.store(&ctx.paths.state)?;
    Ok(state)
}

async fn install_ollama_windows() -> anyhow::Result<()> {
    // Headless: prefer standalone zip for wrapping `ollama serve`. Desktop: Setup.exe silent.
    if std::env::var("OLLAMA_NODE_AGENT_WINDOWS_ZIP")
        .ok()
        .is_some_and(|v| v == "1")
    {
        download_and_unzip().await
    } else {
        download_setup_silent().await
    }
}

async fn download_setup_silent() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(120))
        .build()?;
    let bytes = client.get(SETUP_URL).send().await?.error_for_status()?.bytes().await?;
    let path = std::env::temp_dir().join("OllamaSetup.exe");
    std::fs::write(&path, &bytes)?;
    let status = Command::new(&path).args(SETUP_SILENT_ARGS).status().await?;
    if !status.success() {
        anyhow::bail!("OllamaSetup.exe silent install failed");
    }
    Ok(())
}

async fn download_and_unzip() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(120))
        .build()?;
    let bytes = client.get(ZIP_URL).send().await?.error_for_status()?.bytes().await?;
    let zip_path = std::env::temp_dir().join("ollama-windows-amd64.zip");
    std::fs::write(&zip_path, &bytes)?;
    let dest = Path::new(r"C:\Program Files\Ollama");
    std::fs::create_dir_all(dest)?;
    let status = Command::new("tar")
        .args(["-xf", zip_path.to_str().unwrap_or(""), "-C", dest.to_str().unwrap_or("")])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("extract ollama-windows-amd64.zip failed");
    }
    Ok(())
}

async fn install_self_binary(dest: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&exe, dest)?;
    Ok(())
}

async fn register_scheduled_task(exe: &Path) -> anyhow::Result<()> {
    let exe_s = exe.display().to_string();
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", "ollama-node-agent", "/F"])
        .status()
        .await;
    let status = Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            "ollama-node-agent",
            "/SC",
            "ONSTART",
            "/RL",
            "HIGHEST",
            "/RU",
            "SYSTEM",
            "/TR",
            &format!("\"{exe_s}\" serve"),
            "/F",
        ])
        .status()
        .await?;
    if !status.success() {
        tracing::warn!("schtasks create failed; start `ollama-node-agent serve` at login");
    }
    Ok(())
}

async fn set_firewall(_bind: &str) -> anyhow::Result<()> {
    let _ = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            "name=ollama-node-agent-11436",
            "dir=in",
            "action=allow",
            "protocol=TCP",
            "localport=11436",
        ])
        .status()
        .await;
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

    #[test]
    fn silent_flags_match_official_install_ps1() {
        assert!(SETUP_SILENT_ARGS.contains(&"/VERYSILENT"));
        assert!(SETUP_SILENT_ARGS.contains(&"/NORESTART"));
        assert!(SETUP_SILENT_ARGS.contains(&"/SUPPRESSMSGBOXES"));
    }
}
