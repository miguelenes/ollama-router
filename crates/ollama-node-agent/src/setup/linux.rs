//! Linux systemd converge.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use super::{write_bytes_idempotent, write_token_file, ConvergeState, SetupContext};
use crate::collect::ollama_version;

const INSTALL_SH: &str = "https://ollama.com/install.sh";

pub async fn converge(
    ctx: &SetupContext<'_>,
    ollama_bind: &str,
    agent_ip: IpAddr,
) -> anyhow::Result<ConvergeState> {
    if !Path::new("/run/systemd/system").exists() && !ctx.dry_commands {
        anyhow::bail!("systemd is required on Linux (no /run/systemd/system)");
    }

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

    let agent_unit = agent_unit_text();
    let unit_changed = write_bytes_idempotent(
        &ctx.paths.unit_dir.join("ollama-node-agent.service"),
        agent_unit.as_bytes(),
    )?;
    tracing::info!(unit_changed, "agent systemd unit");
    state.unit_written = true;

    if ctx.config.tailscale.enable {
        if let Some(key) = ctx.ts_authkey.filter(|k| !k.is_empty()) {
            join_tailscale(key, ctx.dry_commands).await?;
        } else {
            tracing::info!("tailscale.enable but no auth key; skip join");
        }
    }

    if !ctx.dry_commands {
        ensure_agent_user().await?;
        install_self_binary(&ctx.paths.bin_dir.join("ollama-node-agent")).await?;
        systemctl(&["daemon-reload"]).await?;
        systemctl(&["enable", "--now", "ollama"]).await?;
        wait_ollama_tags(ollama_bind).await?;
        systemctl(&["enable", "--now", "ollama-node-agent"]).await?;
    }

    let _ = agent_ip;
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

fn agent_unit_text() -> String {
    r#"[Unit]
Description=Ollama node agent
After=network-online.target ollama.service
Wants=network-online.target

[Service]
Type=simple
User=ollama-node-agent
ExecStart=/usr/local/bin/ollama-node-agent serve --config /etc/ollama-node-agent/config.yaml
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
EnvironmentFile=-/etc/ollama-node-agent/env

[Install]
WantedBy=multi-user.target
"#
    .into()
}

async fn install_ollama() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(60))
        .build()?;
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

async fn install_self_binary(dest: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        if let (Ok(a), Ok(b)) = (std::fs::read(&exe), std::fs::read(dest)) {
            if a == b {
                return Ok(());
            }
        }
    }
    std::fs::copy(&exe, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

async fn wait_ollama_tags(bind: &str) -> anyhow::Result<()> {
    let url = format!("http://{bind}/api/tags");
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(2))
        .build()?;
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

async fn join_tailscale(authkey: &str, dry: bool) -> anyhow::Result<()> {
    if dry {
        return Ok(());
    }
    let fut = Command::new("tailscale")
        .args(["up", "--authkey", authkey, "--ssh"])
        .output();
    match timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        _ => {
            tracing::warn!("tailscale up failed or timed out");
            Ok(())
        }
    }
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
        let text = ollama_dropin("100.64.1.2:11434", Some("/var/lib/ollama/models"), &extra);
        assert!(text.contains("Environment=OLLAMA_HOST=100.64.1.2:11434"));
        assert!(text.contains("Environment=OLLAMA_MODELS=/var/lib/ollama/models"));
        assert!(text.contains("OLLAMA_NUM_PARALLEL=2"));
        assert_eq!(text.matches("OLLAMA_HOST=").count(), 1);
    }

    #[test]
    fn unit_has_nonewprivileges() {
        let u = agent_unit_text();
        assert!(u.contains("NoNewPrivileges=true"));
        assert!(u.contains("User=ollama-node-agent"));
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
            ts_authkey: None,
            dry_commands: true,
        };
        let state = converge(&ctx, "127.0.0.1:11434", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .await
            .unwrap();
        assert!(state.unit_written);
        assert!(paths.state.exists());
        let unit =
            std::fs::read_to_string(paths.unit_dir.join("ollama-node-agent.service")).unwrap();
        assert!(unit.contains("NoNewPrivileges=true"));
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
    }
}
