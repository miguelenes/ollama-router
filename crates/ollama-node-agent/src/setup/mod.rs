//! Idempotent converge: detect → install Ollama → write listen/env → enable services.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod state;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

use crate::config::AgentConfig;
use crate::listen::{format_host_port, resolve_bind, AddrSource, HostAddrs};
use crate::redact::redact_authkey;

pub use state::{ConvergeState, STATE_SCHEMA};

/// Official Inno flags from ollama `scripts/install.ps1`.
pub fn windows_silent_args() -> &'static [&'static str] {
    &["/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES"]
}

#[derive(Clone, Debug)]
pub struct SetupPaths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub token_file: PathBuf,
    pub unit_dir: PathBuf,
    pub bin_dir: PathBuf,
}

impl SetupPaths {
    pub fn for_os() -> Self {
        if cfg!(windows) {
            let root = PathBuf::from(r"C:\ProgramData\ollama-node-agent");
            Self {
                config: root.join("config.yaml"),
                state: root.join("state.json"),
                token_file: root.join("token"),
                unit_dir: root.clone(),
                bin_dir: PathBuf::from(r"C:\Program Files\ollama-node-agent"),
            }
        } else if cfg!(target_os = "macos") {
            let root = PathBuf::from("/Library/Application Support/ollama-node-agent");
            Self {
                config: root.join("config.yaml"),
                state: root.join("state.json"),
                token_file: root.join("token"),
                unit_dir: PathBuf::from("/Library/LaunchDaemons"),
                bin_dir: PathBuf::from("/usr/local/bin"),
            }
        } else {
            Self {
                config: PathBuf::from("/etc/ollama-node-agent/config.yaml"),
                state: PathBuf::from("/var/lib/ollama-node-agent/state.json"),
                token_file: PathBuf::from("/etc/ollama-node-agent/token"),
                unit_dir: PathBuf::from("/etc/systemd/system"),
                bin_dir: PathBuf::from("/usr/local/bin"),
            }
        }
    }

    pub fn under_root(root: &Path) -> Self {
        Self {
            config: root.join("etc/ollama-node-agent/config.yaml"),
            state: root.join("var/lib/ollama-node-agent/state.json"),
            token_file: root.join("etc/ollama-node-agent/token"),
            unit_dir: root.join("etc/systemd/system"),
            bin_dir: root.join("usr/local/bin"),
        }
    }
}

pub struct SetupContext<'a> {
    pub config: &'a AgentConfig,
    pub paths: &'a SetupPaths,
    pub ts_authkey: Option<&'a str>,
    pub dry_commands: bool,
}

pub async fn run(ctx: SetupContext<'_>) -> anyhow::Result<ConvergeState> {
    tracing::info!("setup detect");
    let token_set = ctx.config.bearer_token().is_some();
    let addrs = HostAddrs;
    let addrs: &dyn AddrSource = &addrs;
    let agent_ip = resolve_bind(&ctx.config.listen, addrs, token_set)?;
    let ollama_ip = resolve_bind(&ctx.config.ollama.listen, addrs, token_set)?;
    let ollama_bind = format_host_port(ollama_ip, 11434);
    tracing::info!(ollama_bind = %ollama_bind, agent_ip = %agent_ip, "setup bind");
    if let Some(key) = ctx.ts_authkey {
        tracing::info!(ts_authkey = %redact_authkey(Some(key)), "setup tailscale key present");
    }

    #[cfg(target_os = "linux")]
    {
        linux::converge(&ctx, &ollama_bind, agent_ip).await
    }
    #[cfg(target_os = "macos")]
    {
        macos::converge(&ctx, &ollama_bind, agent_ip).await
    }
    #[cfg(windows)]
    {
        windows::converge(&ctx, &ollama_bind, agent_ip).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (ctx, ollama_bind, agent_ip);
        anyhow::bail!("unsupported OS for setup")
    }
}

/// Write token file 0600 when a bearer is configured.
pub fn write_token_file(path: &Path, token: Option<&str>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match token.filter(|t| !t.is_empty()) {
        Some(t) => {
            std::fs::write(path, t)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

pub fn write_bytes_idempotent(path: &Path, contents: &[u8]) -> anyhow::Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if let Ok(existing) = std::fs::read(path) {
            if existing == contents {
                return Ok(false);
            }
        }
    }
    std::fs::write(path, contents)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn token_file_mode_and_skip_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("token");
        write_token_file(&path, Some("secret")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        write_token_file(&path, None).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn idempotent_write_skips_same_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unit");
        assert!(write_bytes_idempotent(&path, b"abc").unwrap());
        assert!(!write_bytes_idempotent(&path, b"abc").unwrap());
        assert!(write_bytes_idempotent(&path, b"abcd").unwrap());
    }

    #[test]
    fn windows_silent_flags_documented() {
        assert_eq!(
            crate::setup::windows_silent_args(),
            &["/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES"]
        );
    }
}
