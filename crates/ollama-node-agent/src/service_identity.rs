//! Shared Windows SCM / MSI identity. Keep in sync with
//! `packaging/windows/ollama-node-agent.wxs`.

pub const SERVICE_NAME: &str = "ollama-node-agent";
pub const SERVICE_DISPLAY_NAME: &str = "Ollama Node Agent";
pub const TUNNEL_SERVICE_NAME: &str = "ollama-node-agent-tunnel";
pub const TUNNEL_SERVICE_DISPLAY_NAME: &str = "Ollama Node Agent Tunnel";
pub const WINDOWS_BIN: &str = r"C:\Program Files\ollama-node-agent\ollama-node-agent.exe";
pub const WINDOWS_CONFIG: &str = r"C:\ProgramData\ollama-node-agent\config.yaml";
pub const WINDOWS_SERVICE_ARGS: &str =
    r#"serve --windows-service --config "C:\ProgramData\ollama-node-agent\config.yaml""#;
pub const WINDOWS_TUNNEL_SERVICE_ARGS: &str =
    r#"tunnel --windows-service --config "C:\ProgramData\ollama-node-agent\config.yaml""#;
pub const UPGRADE_CODE: &str = "9F3A2C10-8B7E-4D61-A5C4-1E8F0B2D6A73";
pub const FIREWALL_RULE_11434: &str = "ollama-node-agent-11434";
pub const FIREWALL_RULE_11436: &str = "ollama-node-agent-11436";

pub fn service_bin_path(exe: &std::path::Path) -> String {
    format!("\"{}\" {}", exe.display(), WINDOWS_SERVICE_ARGS)
}

pub fn tunnel_service_bin_path(exe: &std::path::Path) -> String {
    format!("\"{}\" {}", exe.display(), WINDOWS_TUNNEL_SERVICE_ARGS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn wxs_matches_identity() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows/ollama-node-agent.wxs");
        let wxs = std::fs::read_to_string(path).expect("wxs");
        assert!(wxs.contains(SERVICE_NAME));
        assert!(wxs.contains(SERVICE_DISPLAY_NAME));
        assert!(wxs.contains("LocalSystem"));
        assert!(wxs.contains("--windows-service"));
        assert!(wxs.contains(WINDOWS_CONFIG));
        assert!(wxs.contains(UPGRADE_CODE));
        assert!(wxs.contains(WINDOWS_BIN.rsplit('\\').next().unwrap_or("")));
        assert!(wxs.contains("Start=\"install\""));
        assert!(wxs.contains("Remove=\"uninstall\""));
        assert!(wxs.contains("NeverOverwrite=\"yes\""));
        assert!(wxs.contains("$(var.ConfigPath)"));
        assert!(!wxs.contains("schtasks"));
        assert!(!wxs.contains("FirewallException"));
        assert!(!wxs.to_lowercase().contains("ollama.com"));
    }

    #[test]
    fn linux_unit_file_is_canonical() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/linux/ollama-node-agent.service");
        let on_disk = std::fs::read_to_string(path).expect("unit");
        assert_eq!(on_disk, crate::setup::agent_unit_text());
        assert!(on_disk.contains("NoNewPrivileges=true"));
        assert!(on_disk.contains("ExecStart=/usr/local/bin/ollama-node-agent"));
        let tunnel_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("packaging/linux/ollama-node-agent-tunnel.service");
        let tunnel_on_disk = std::fs::read_to_string(tunnel_path).expect("tunnel unit");
        assert_eq!(tunnel_on_disk, crate::setup::tunnel_unit_text());
        assert!(tunnel_on_disk.contains("ExecStart=/usr/local/bin/ollama-node-agent tunnel"));
    }

    #[test]
    fn postinst_enables_systemd_and_skips_ollama_install() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/linux/postinst.sh");
        let script = std::fs::read_to_string(path).expect("postinst");
        assert!(script.contains("/run/systemd/system"));
        assert!(script.contains("enable --now ollama-node-agent"));
        assert!(!script.contains("install.sh"));
        assert!(!script.to_lowercase().contains("ollama.com"));
    }

    #[test]
    fn openrc_contrib_uses_same_bin_and_config() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/linux/openrc/ollama-node-agent");
        let script = std::fs::read_to_string(path).expect("openrc");
        assert!(script.contains("/usr/local/bin/ollama-node-agent"));
        assert!(script.contains("/etc/ollama-node-agent/config.yaml"));
    }

    #[test]
    fn macos_agent_plist_matches_binary_print() {
        assert_eq!(
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("packaging/macos/com.ollama.node-agent.plist")
            )
            .expect("agent plist"),
            crate::setup::agent_plist_text()
        );
    }

    #[test]
    fn macos_tunnel_plist_matches_binary_print() {
        assert_eq!(
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("packaging/macos/com.ollama.node-agent.tunnel.plist")
            )
            .expect("tunnel plist"),
            crate::setup::tunnel_plist()
        );
    }

    #[test]
    fn macos_plist_file_is_canonical() {
        let on_disk = crate::setup::agent_plist_text();
        assert!(on_disk.contains("com.ollama.node-agent"));
        assert!(on_disk.contains("/Library/Application Support/ollama-node-agent/config.yaml"));
        assert!(!on_disk.contains("OLLAMA_NODE_AGENT_HOST"));
        assert!(on_disk.contains("<key>KeepAlive</key>"));
        assert!(on_disk.contains("<key>RunAtLoad</key>"));
        assert!(on_disk.contains("/usr/local/bin/ollama-node-agent"));
        let tunnel_on_disk = crate::setup::tunnel_plist();
        assert!(tunnel_on_disk.contains("com.ollama.node-agent.tunnel"));
        assert!(tunnel_on_disk.contains("<key>Disabled</key>"));
    }

    #[test]
    fn pack_pkg_agent_plist_drift_would_fail_cmp() {
        let canonical = crate::setup::agent_plist_text();
        let drifted = format!("<!-- drift -->\n{canonical}");
        let dir = tempfile::tempdir().expect("tempdir");
        let generated = dir.path().join("generated.plist");
        let checked_in = dir.path().join("checked-in.plist");
        std::fs::write(&generated, canonical).expect("write generated");
        std::fs::write(&checked_in, &drifted).expect("write drifted");
        let status = std::process::Command::new("cmp")
            .args([
                "-s",
                checked_in.to_str().unwrap(),
                generated.to_str().unwrap(),
            ])
            .status()
            .expect("cmp");
        assert!(!status.success());
    }

    #[test]
    fn macos_postinstall_bootstraps_agent_not_tunnel() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/macos/scripts/postinstall");
        let script = std::fs::read_to_string(path).expect("postinstall");
        assert!(script.contains("launchctl bootout system/com.ollama.node-agent"));
        assert!(script.contains("launchctl bootout system/com.ollama.node-agent.tunnel"));
        assert!(script.contains("launchctl bootstrap system"));
        assert!(!script.contains("com.ollama.node-agent.tunnel.plist"));
        assert!(!script.contains("brew"));
        assert!(!script.contains("Ollama.app"));
        assert!(!script.to_lowercase().contains("ollama.com"));
    }

    #[test]
    fn service_bin_path_matches_identity() {
        let exe = Path::new(WINDOWS_BIN);
        let path = service_bin_path(exe);
        assert!(path.contains("serve --windows-service"));
        assert!(path.contains(WINDOWS_CONFIG));
        assert!(path.starts_with('"'));
        assert_eq!(SERVICE_NAME, "ollama-node-agent");
        assert_eq!(TUNNEL_SERVICE_NAME, "ollama-node-agent-tunnel");
        assert_eq!(
            path,
            format!("\"{}\" {}", WINDOWS_BIN, WINDOWS_SERVICE_ARGS)
        );
        assert_eq!(
            tunnel_service_bin_path(exe),
            format!("\"{}\" {}", WINDOWS_BIN, WINDOWS_TUNNEL_SERVICE_ARGS)
        );
    }
}
