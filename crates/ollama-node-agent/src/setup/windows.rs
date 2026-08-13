//! Windows: silent Setup.exe (desktop) or standalone zip (headless service).
//! GPU NVIDIA under a service often needs LocalSystem; CPU-only can be LocalService.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::{
    install_self_binary, write_bytes_idempotent, write_token_file, ConvergeState, SetupContext,
    SUPERVISOR_MANUAL, SUPERVISOR_SCM,
};
use crate::collect::ollama_version;
use crate::service_identity::{
    service_bin_path, FIREWALL_RULE_11434, FIREWALL_RULE_11436, SERVICE_DISPLAY_NAME, SERVICE_NAME,
};

/// Inno Setup flags from ollama/ollama `scripts/install.ps1`.
pub const SETUP_SILENT_ARGS: &[&str] = &["/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES"];

const SETUP_URL: &str = "https://ollama.com/download/OllamaSetup.exe";
const ZIP_URL: &str =
    "https://github.com/ollama/ollama/releases/latest/download/ollama-windows-amd64.zip";

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
    let mut env_body = format!("OLLAMA_HOST={ollama_bind}\n");
    if let Some(dir) = ctx
        .config
        .ollama
        .models_dir
        .as_deref()
        .filter(|d| !d.is_empty())
    {
        env_body.push_str(&format!("OLLAMA_MODELS={dir}\n"));
    }
    write_bytes_idempotent(&ctx.paths.unit_dir.join("ollama.env"), env_body.as_bytes())?;

    tracing::info!(
        agent_ip = %agent_ip,
        "Windows: do not also run the tray app on :11434; NVIDIA in a service usually needs LocalSystem"
    );

    if !ctx.dry_commands {
        let dest = ctx.paths.bin_dir.join("ollama-node-agent.exe");
        stop_windows_service().await;
        install_self_binary(&dest)?;
        let registered = register_windows_service(&dest).await?;
        state.unit_written = registered;
        state.supervisor = Some(if registered {
            SUPERVISOR_SCM.into()
        } else {
            SUPERVISOR_MANUAL.into()
        });
        let _ = set_firewall().await;
    } else {
        state.unit_written = true;
        state.supervisor = Some(SUPERVISOR_SCM.into());
    }

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
    let bytes = client
        .get(SETUP_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
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
    let bytes = client
        .get(ZIP_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let zip_path = std::env::temp_dir().join("ollama-windows-amd64.zip");
    std::fs::write(&zip_path, &bytes)?;
    let dest = Path::new(r"C:\Program Files\Ollama");
    std::fs::create_dir_all(dest)?;
    let status = Command::new("tar")
        .args([
            "-xf",
            zip_path.to_str().unwrap_or(""),
            "-C",
            dest.to_str().unwrap_or(""),
        ])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("extract ollama-windows-amd64.zip failed");
    }
    Ok(())
}

async fn delete_legacy_scheduled_task() {
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", SERVICE_NAME, "/F"])
        .status()
        .await;
}

async fn stop_windows_service() {
    delete_legacy_scheduled_task().await;
    let _ = Command::new("sc.exe")
        .args(["stop", SERVICE_NAME])
        .status()
        .await;
}

async fn register_windows_service(exe: &Path) -> anyhow::Result<bool> {
    let bin_path = service_bin_path(exe);
    let exists = Command::new("sc.exe")
        .args(["query", SERVICE_NAME])
        .status()
        .await
        .is_ok_and(|s| s.success());
    let status = if exists {
        Command::new("sc.exe")
            .args([
                "config",
                SERVICE_NAME,
                "binPath=",
                &bin_path,
                "start=",
                "auto",
                "obj=",
                "LocalSystem",
                "DisplayName=",
                SERVICE_DISPLAY_NAME,
            ])
            .status()
            .await?
    } else {
        Command::new("sc.exe")
            .args([
                "create",
                SERVICE_NAME,
                "binPath=",
                &bin_path,
                "start=",
                "auto",
                "obj=",
                "LocalSystem",
                "DisplayName=",
                SERVICE_DISPLAY_NAME,
            ])
            .status()
            .await?
    };
    if !status.success() {
        tracing::warn!("sc create/config failed; start `ollama-node-agent serve` as LocalSystem");
        return Ok(false);
    }
    let _ = Command::new("sc.exe")
        .args(["start", SERVICE_NAME])
        .status()
        .await;
    Ok(true)
}

async fn set_firewall() -> anyhow::Result<()> {
    for (port, name) in [
        ("11434", FIREWALL_RULE_11434),
        ("11436", FIREWALL_RULE_11436),
    ] {
        let _ = Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={name}"),
                "dir=in",
                "action=allow",
                "protocol=TCP",
                &format!("localport={port}"),
            ])
            .status()
            .await;
    }
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
