//! Best-effort removal of the agent service. Ollama stays unless `--purge-ollama`.

use tokio::process::Command;

use crate::config::AgentConfig;
use crate::setup::SetupPaths;

pub async fn run(_config: &AgentConfig, purge_ollama: bool) -> anyhow::Result<()> {
    let paths = SetupPaths::for_os();
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("systemctl")
            .args(["disable", "--now", "ollama-node-agent"])
            .status()
            .await;
        let _ = std::fs::remove_file(paths.unit_dir.join("ollama-node-agent.service"));
        let _ = std::fs::remove_file(
            paths
                .unit_dir
                .join("ollama.service.d/ollama-node-agent.conf"),
        );
        let _ = Command::new("systemctl")
            .args(["daemon-reload"])
            .status()
            .await;
        if purge_ollama {
            tracing::warn!("--purge-ollama: not removing the ollama package automatically");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let plist = paths.unit_dir.join("com.ollama.node-agent.plist");
        let _ = Command::new("launchctl")
            .args(["bootout", "system/com.ollama.node-agent"])
            .status()
            .await;
        let _ = std::fs::remove_file(&plist);
        let _ = purge_ollama;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", "ollama-node-agent", "/F"])
            .status()
            .await;
        let _ = purge_ollama;
    }
    let _ = std::fs::remove_file(&paths.state);
    tracing::info!("uninstall complete (Ollama left in place unless operator removes it)");
    Ok(())
}
