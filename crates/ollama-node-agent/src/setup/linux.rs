//! Linux systemd converge.

use std::net::IpAddr;
use std::time::Duration;

use tokio::process::Command;

use super::{
    install_self_binary, tunnel, write_bytes_idempotent, write_token_file, ConvergeState,
    SetupContext, SUPERVISOR_MANUAL, SUPERVISOR_SYSTEMD,
};
use crate::collect::ollama_version;

const INSTALL_SH: &str = "https://ollama.com/install.sh";

pub async fn converge(
    ctx: &SetupContext<'_>,
    ollama_bind: &str,
    agent_ip: IpAddr,
) -> anyhow::Result<ConvergeState> {
    let systemd = systemd_available(ctx.dry_commands);

    let mut state = ConvergeState::load(&ctx.paths.state);
    state.schema = super::STATE_SCHEMA;
    state.listen_mode = Some(format!("{:?}", ctx.config.listen));
    state.bind = Some(ollama_bind.to_string());

    let version = ollama_version().await;
    if version.is_some() {
        tracing::info!("ollama already installed; skip download");
        state.ollama_installed = true;
        state.ollama_version = version.clone();
    } else if ctx.dry_commands {
        tracing::info!("dry-run: skip ollama install.sh");
    } else {
        install_ollama().await?;
        state.ollama_installed = true;
        state.ollama_version = ollama_version().await;
    }

    write_token_file(&ctx.paths.token_file, ctx.config.bearer_token())?;

    if systemd {
        let dropin_dir = ctx.paths.unit_dir.join("ollama.service.d");
        std::fs::create_dir_all(&dropin_dir)?;
        let dropin = ollama_dropin(
            ollama_bind,
            ctx.config.ollama.models_dir.as_deref(),
            &ctx.config.ollama.extra_env,
        );
        write_bytes_idempotent(
            &dropin_dir.join("ollama-node-agent.conf"),
            dropin.as_bytes(),
        )?;

        let unit_changed = write_bytes_idempotent(
            &ctx.paths.unit_dir.join("ollama-node-agent.service"),
            super::agent_unit_text().as_bytes(),
        )?;
        tracing::info!(unit_changed, "agent systemd unit");
        state.unit_written = true;
        state.supervisor = Some(SUPERVISOR_SYSTEMD.into());
        if ctx.config.tunnel.enable {
            let tunnel_changed = write_bytes_idempotent(
                &ctx.paths.unit_dir.join("ollama-node-agent-tunnel.service"),
                tunnel::tunnel_unit_text().as_bytes(),
            )?;
            tracing::info!(tunnel_changed, "tunnel systemd unit");
            state.tunnel_unit_written = true;
        }
    } else {
        tracing::info!(
            config = %ctx.paths.config.display(),
            "systemd not detected; binary will be installed without a unit"
        );
        state.unit_written = false;
        state.supervisor = Some(SUPERVISOR_MANUAL.into());
    }

    if ctx.dry_commands && ctx.config.tunnel.enable {
        tunnel::prepare_shares(ctx.config, ctx.paths, &mut state, ctx.enable_token, true).await?;
    }

    if !ctx.dry_commands {
        let dest = ctx.paths.bin_dir.join("ollama-node-agent");
        if systemd {
            ensure_agent_user().await?;
            let _ = Command::new("systemctl")
                .args(["stop", "ollama-node-agent"])
                .status()
                .await;
            let _ = Command::new("systemctl")
                .args(["stop", tunnel::TUNNEL_UNIT_NAME])
                .status()
                .await;
        }
        install_self_binary(&dest)?;
        if systemd {
            systemctl(&["daemon-reload"]).await?;
            systemctl(&["enable", "--now", "ollama"]).await?;
            wait_ollama_tags(ollama_bind).await?;
            if ctx.config.tunnel.enable {
                tunnel::wait_ollama_loopback_tags().await?;
                tunnel::prepare_shares(ctx.config, ctx.paths, &mut state, ctx.enable_token, false)
                    .await?;
                chown_agent_path(&tunnel::zrok_home(ctx.paths)).await;
                chown_agent_path(&ctx.paths.state).await;
                chown_agent_path(&tunnel::find_path(ctx.paths)).await;
            }
            systemctl(&["enable", "--now", "ollama-node-agent"]).await?;
            if ctx.config.tunnel.enable {
                systemctl(&["enable", "--now", tunnel::TUNNEL_UNIT_NAME]).await?;
            }
        } else {
            tracing::info!(
                bin = %dest.display(),
                config = %ctx.paths.config.display(),
                "no systemd; start under your supervisor: {} serve --config {}",
                dest.display(),
                ctx.paths.config.display()
            );
            if ctx.config.tunnel.enable {
                tracing::info!(
                    "start tunnel sidecar: {} tunnel --config {}",
                    dest.display(),
                    ctx.paths.config.display()
                );
                tunnel::prepare_shares(ctx.config, ctx.paths, &mut state, ctx.enable_token, false)
                    .await?;
            }
        }
    }

    tracing::info!(agent_ip = %agent_ip, systemd, "linux setup converge done");
    state.last_converge = Some(now_rfc3339());
    state.store(&ctx.paths.state)?;
    Ok(state)
}

fn ollama_dropin(
    bind: &str,
    models_dir: Option<&str>,
    extra: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut out = String::from("[Service]\n");
    out.push_str(&format!("Environment=OLLAMA_HOST={bind}\n"));
    if let Some(dir) = models_dir.filter(|d| !d.is_empty()) {
        out.push_str(&format!("Environment=OLLAMA_MODELS={dir}\n"));
    }
    for (k, v) in extra {
        if k.eq_ignore_ascii_case("OLLAMA_HOST") || k.eq_ignore_ascii_case("OLLAMA_MODELS") {
            continue;
        }
        out.push_str(&format!("Environment={k}={v}\n"));
    }
    out
}

fn systemd_available(dry_commands: bool) -> bool {
    dry_commands || super::systemd_detected()
}

async fn install_ollama() -> anyhow::Result<()> {
    let client = crate::rustls_client(None, Some(Duration::from_secs(60)))?;
    let bytes = client
        .get(INSTALL_SH)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let path = std::env::temp_dir().join("ollama-install.sh");
    std::fs::write(&path, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    let status = Command::new("sh").arg(&path).status().await?;
    if !status.success() {
        anyhow::bail!("ollama install.sh failed");
    }
    Ok(())
}

async fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl").args(args).status().await?;
    if !status.success() {
        anyhow::bail!("systemctl {args:?} failed");
    }
    Ok(())
}

async fn ensure_agent_user() -> anyhow::Result<()> {
    let check = Command::new("id").arg("ollama-node-agent").status().await?;
    if check.success() {
        return Ok(());
    }
    let status = Command::new("useradd")
        .args(["-r", "-s", "/usr/sbin/nologin", "-M", "ollama-node-agent"])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("useradd ollama-node-agent failed");
    }
    Ok(())
}

async fn chown_agent_path(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let _ = Command::new("chown")
        .args([
            "-R",
            "ollama-node-agent:ollama-node-agent",
            &path.display().to_string(),
        ])
        .status()
        .await;
}

async fn wait_ollama_tags(bind: &str) -> anyhow::Result<()> {
    let url = format!("http://{bind}/api/tags");
    let client = crate::rustls_client(None, Some(Duration::from_secs(2)))?;
    for _ in 0..30 {
        if client
            .get(&url)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("ollama /api/tags did not become ready on {bind}");
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn dropin_sets_host_and_skips_duplicate() {
        let mut extra = BTreeMap::new();
        extra.insert("OLLAMA_NUM_PARALLEL".into(), "2".into());
        extra.insert("OLLAMA_HOST".into(), "0.0.0.0:11434".into());
        let text = ollama_dropin("127.0.0.1:11434", Some("/var/lib/ollama/models"), &extra);
        assert!(text.contains("Environment=OLLAMA_HOST=127.0.0.1:11434"));
        assert!(text.contains("Environment=OLLAMA_MODELS=/var/lib/ollama/models"));
        assert!(text.contains("OLLAMA_NUM_PARALLEL=2"));
        assert_eq!(text.matches("OLLAMA_HOST=").count(), 1);
    }

    #[test]
    fn unit_has_nonewprivileges() {
        let u = crate::setup::agent_unit_text();
        assert!(u.contains("NoNewPrivileges=true"));
        assert!(u.contains("User=ollama-node-agent"));
        assert!(u.contains("ExecStart=/usr/local/bin/ollama-node-agent"));
        let t = crate::setup::tunnel_unit_text();
        assert!(t.contains("NoNewPrivileges=true"));
        assert!(t.contains("ollama-node-agent-tunnel"));
        assert!(t.contains("HOME=/var/lib/ollama-node-agent"));
        assert!(t.contains("EnvironmentFile=-/etc/ollama-node-agent/env"));
    }

    #[test]
    fn dry_commands_count_as_systemd_for_unit_writes() {
        assert!(systemd_available(true));
    }

    #[tokio::test]
    async fn dry_converge_writes_state_under_temp_root() {
        use crate::config::AgentConfig;
        use crate::setup::{SetupContext, SetupPaths};
        use std::net::{IpAddr, Ipv4Addr};

        let dir = tempfile::tempdir().unwrap();
        let paths = SetupPaths::under_root(dir.path());
        let cfg = AgentConfig::default();
        let ctx = SetupContext {
            config: &cfg,
            paths: &paths,
            enable_token: None,
            dry_commands: true,
        };
        let state = converge(&ctx, "127.0.0.1:11434", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .await
            .unwrap();
        assert!(state.unit_written);
        assert!(state.tunnel_unit_written);
        assert!(state.share_present());
        assert_eq!(state.supervisor.as_deref(), Some(SUPERVISOR_SYSTEMD));
        assert!(paths.state.exists());
        let unit =
            std::fs::read_to_string(paths.unit_dir.join("ollama-node-agent.service")).unwrap();
        assert!(unit.contains("NoNewPrivileges=true"));
        let tunnel_unit =
            std::fs::read_to_string(paths.unit_dir.join("ollama-node-agent-tunnel.service"))
                .unwrap();
        assert!(tunnel_unit.contains("ollama-node-agent tunnel"));
        assert!(tunnel_unit.contains("NoNewPrivileges=true"));
        let dropin = std::fs::read_to_string(
            paths
                .unit_dir
                .join("ollama.service.d/ollama-node-agent.conf"),
        )
        .unwrap();
        assert!(dropin.contains("OLLAMA_HOST=127.0.0.1:11434"));
        let again = converge(&ctx, "127.0.0.1:11434", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .await
            .unwrap();
        assert!(again.unit_written);
        assert!(again.tunnel_unit_written);
        assert_eq!(again.supervisor.as_deref(), Some(SUPERVISOR_SYSTEMD));
    }

    #[tokio::test]
    async fn dry_converge_skips_zrok_when_tunnel_disabled() {
        use crate::config::{AgentConfig, TunnelSection};
        use crate::setup::{SetupContext, SetupPaths};
        use std::net::{IpAddr, Ipv4Addr};

        let dir = tempfile::tempdir().unwrap();
        let paths = SetupPaths::under_root(dir.path());
        let cfg = AgentConfig {
            tunnel: TunnelSection {
                enable: false,
                ..TunnelSection::default()
            },
            ..AgentConfig::default()
        };
        let ctx = SetupContext {
            config: &cfg,
            paths: &paths,
            enable_token: None,
            dry_commands: true,
        };
        let state = converge(&ctx, "127.0.0.1:11434", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .await
            .unwrap();
        assert!(state.unit_written);
        assert!(!state.tunnel_unit_written);
        assert!(!state.share_present());
        assert!(!paths
            .unit_dir
            .join("ollama-node-agent-tunnel.service")
            .exists());
    }
}
