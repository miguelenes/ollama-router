//! Best-effort removal of the agent service. Ollama stays unless `--purge-ollama`.

use tokio::process::Command;

use crate::config::AgentConfig;
use crate::setup::SetupPaths;

pub async fn run(_config: &AgentConfig, purge_ollama: bool) -> anyhow::Result<()> {
    let paths = SetupPaths::for_os();
    #[cfg(target_os = "linux")]
    {
        if crate::setup::systemd_detected() {
            let _ = Command::new("systemctl")
                .args(["disable", "--now", "ollama-node-agent-tunnel"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["disable", "--now", "ollama-node-agent"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["daemon-reload"])
                .status()
                .await;
        }
        let _ = std::fs::remove_file(paths.unit_dir.join("ollama-node-agent.service"));
        let _ = std::fs::remove_file(paths.unit_dir.join("ollama-node-agent-tunnel.service"));
        let _ = std::fs::remove_file("/usr/lib/systemd/system/ollama-node-agent.service");
        let _ = std::fs::remove_file("/usr/lib/systemd/system/ollama-node-agent-tunnel.service");
        let _ = std::fs::remove_file(
            paths
                .unit_dir
                .join("ollama.service.d/ollama-node-agent.conf"),
        );
        let _ = std::fs::remove_file(paths.bin_dir.join("ollama-node-agent"));
        if purge_ollama {
            tracing::warn!("--purge-ollama: not removing the ollama package automatically");
        }
    }
    #[cfg(target_os = "macos")]
    {
        let plist = paths.unit_dir.join("com.ollama.node-agent.plist");
        let tunnel_plist = paths.unit_dir.join("com.ollama.node-agent.tunnel.plist");
        let _ = Command::new("launchctl")
            .args(["bootout", "system/com.ollama.node-agent.tunnel"])
            .status()
            .await;
        let _ = Command::new("launchctl")
            .args(["bootout", "system/com.ollama.node-agent"])
            .status()
            .await;
        let _ = std::fs::remove_file(&plist);
        let _ = std::fs::remove_file(&tunnel_plist);
        let _ = std::fs::remove_file(paths.bin_dir.join("ollama-node-agent"));
        let _ = purge_ollama;
    }
    #[cfg(windows)]
    {
        use crate::service_identity::{
            FIREWALL_RULE_11434, FIREWALL_RULE_11436, SERVICE_NAME, TUNNEL_SERVICE_NAME,
            WINDOWS_BIN,
        };
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", SERVICE_NAME, "/F"])
            .status()
            .await;
        let _ = Command::new("sc.exe")
            .args(["stop", TUNNEL_SERVICE_NAME])
            .status()
            .await;
        let _ = Command::new("sc.exe")
            .args(["delete", TUNNEL_SERVICE_NAME])
            .status()
            .await;
        let _ = Command::new("sc.exe")
            .args(["stop", SERVICE_NAME])
            .status()
            .await;
        let _ = Command::new("sc.exe")
            .args(["delete", SERVICE_NAME])
            .status()
            .await;
        for name in [FIREWALL_RULE_11434, FIREWALL_RULE_11436] {
            let _ = Command::new("netsh")
                .args([
                    "advfirewall",
                    "firewall",
                    "delete",
                    "rule",
                    &format!("name={name}"),
                ])
                .status()
                .await;
        }
        let _ = std::fs::remove_file(WINDOWS_BIN);
        let _ = std::fs::remove_file(paths.bin_dir.join("ollama-node-agent.exe"));
        let _ = purge_ollama;
    }
    let _ = std::fs::remove_file(&paths.token_file);
    let _ = std::fs::remove_file(&paths.state);
    let _ = std::fs::remove_file(crate::setup::tunnel::find_path(&paths));
    tracing::info!("uninstall complete (Ollama left in place unless operator removes it)");
    Ok(())
}
