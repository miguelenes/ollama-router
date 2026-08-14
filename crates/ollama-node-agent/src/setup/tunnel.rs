//! Spawn the zrok binary as a sidecar. Private shares only. No Go SDK, no TUN.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::AgentConfig;
use crate::redact::{redact_secret, share_token_id};

use super::{write_bytes_idempotent, ConvergeState, SetupPaths};

pub const OLLAMA_LOOPBACK_URL: &str = "http://127.0.0.1:11434";
pub const TUNNEL_UNIT_NAME: &str = "ollama-node-agent-tunnel";
pub const TUNNEL_PLIST_LABEL: &str = "com.ollama.node-agent.tunnel";

pub fn tunnel_unit_text() -> &'static str {
    include_str!("../../packaging/linux/ollama-node-agent-tunnel.service")
}

pub fn tunnel_plist() -> &'static str {
    include_str!("../../packaging/macos/com.ollama.node-agent.tunnel.plist")
}

pub fn agent_loopback_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn zrok_home(paths: &SetupPaths) -> PathBuf {
    paths
        .state
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.state.clone())
}

pub fn find_path(paths: &SetupPaths) -> PathBuf {
    zrok_home(paths).join("find.json")
}

pub fn file_url(path: &Path) -> String {
    let rendered = path.display().to_string().replace('\\', "/");
    if cfg!(windows) {
        format!("file:///{rendered}")
    } else {
        format!("file://{rendered}")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShareTokens {
    pub ollama: String,
    pub agent: String,
}

pub fn derive_agent_share_token(ollama: &str) -> String {
    format!("{ollama}-agent")
}

pub fn reserve_private_args(unique_name: Option<&str>, target: &str) -> Vec<String> {
    let mut args = vec![
        "reserve".into(),
        "private".into(),
        "--headless".into(),
        "--backend-mode".into(),
        "proxy".into(),
    ];
    if let Some(name) = unique_name.filter(|s| !s.is_empty()) {
        args.push("--unique-name".into());
        args.push(name.to_string());
    }
    args.push(target.into());
    args
}

pub fn share_reserved_args(token: &str) -> Vec<String> {
    vec![
        "share".into(),
        "reserved".into(),
        "--headless".into(),
        token.into(),
    ]
}

pub fn parse_share_token(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                for key in ["token", "shareToken", "share_token", "Token"] {
                    if let Some(token) = value.get(key).and_then(|v| v.as_str()) {
                        let token = token.trim();
                        if !token.is_empty() {
                            return Some(token.to_string());
                        }
                    }
                }
            }
        }
        let lower = trimmed.to_ascii_lowercase();
        for needle in [
            "your private share token is:",
            "share token:",
            "access private ",
        ] {
            if let Some(idx) = lower.find(needle) {
                let rest = trimmed[idx + needle.len()..].trim();
                let token = rest.split_whitespace().next().unwrap_or("");
                if !token.is_empty() {
                    return Some(token.trim_matches(|c| c == '"' || c == '\'').to_string());
                }
            }
        }
    }
    None
}

pub fn read_token_file(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'));
    Ok(line.map(str::to_string))
}

pub fn resolve_configured_tokens(config: &AgentConfig) -> anyhow::Result<ShareTokens> {
    let mut ollama = config
        .tunnel
        .share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if ollama.is_none() {
        if let Some(path) = config
            .tunnel
            .share_token_file
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            ollama = read_token_file(Path::new(path))?;
        }
    }
    let agent = config
        .tunnel
        .agent_share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| ollama.as_deref().map(derive_agent_share_token));
    Ok(ShareTokens {
        ollama: ollama.unwrap_or_default(),
        agent: agent.unwrap_or_default(),
    })
}

pub fn merge_tokens(configured: ShareTokens, state: &ConvergeState) -> ShareTokens {
    ShareTokens {
        ollama: first_non_empty(&configured.ollama, state.ollama_share_token.as_deref()),
        agent: first_non_empty(&configured.agent, state.agent_share_token.as_deref()),
    }
}

fn first_non_empty(primary: &str, fallback: Option<&str>) -> String {
    if !primary.is_empty() {
        return primary.to_string();
    }
    fallback
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn write_find_file(path: &Path, tokens: &ShareTokens) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "ollama_share_id": tokens.ollama,
        "agent_share_id": tokens.agent,
    });
    write_bytes_idempotent(path, serde_json::to_vec_pretty(&body)?.as_slice())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn zrok_bin_present(bin: &str) -> bool {
    which_bin(bin).is_some()
}

fn which_bin(bin: &str) -> Option<PathBuf> {
    let candidate = Path::new(bin);
    if candidate.is_absolute() || bin.contains('/') || bin.contains('\\') {
        return candidate.exists().then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(bin);
        if p.is_file() {
            return Some(p);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{bin}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn apply_api_endpoint(cmd: &mut Command, endpoint: Option<&str>) {
    if let Some(ep) = endpoint.map(str::trim).filter(|s| !s.is_empty()) {
        cmd.env("ZROK_API_ENDPOINT", ep);
    }
}

fn configured_api_endpoint(config: &AgentConfig) -> Option<&str> {
    config
        .tunnel
        .api_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub async fn maybe_enable(
    bin: &str,
    home: &Path,
    token: Option<&str>,
    api_endpoint: Option<&str>,
    dry: bool,
) -> anyhow::Result<()> {
    let Some(token) = token.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    tracing::info!(enable_token = %redact_secret(Some(token)), "zrok enable token present");
    if dry {
        return Ok(());
    }
    let mut cmd = Command::new(bin);
    cmd.args(["enable", token])
        .env("HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_api_endpoint(&mut cmd, api_endpoint);
    let status = cmd.status().await?;
    if !status.success() {
        tracing::warn!("zrok enable failed or already enabled");
    }
    Ok(())
}

pub async fn reserve_share(
    bin: &str,
    home: &Path,
    unique_name: Option<&str>,
    target: &str,
    api_endpoint: Option<&str>,
    dry: bool,
) -> anyhow::Result<String> {
    if dry {
        return Ok(unique_name.unwrap_or("dry-share").to_string());
    }
    let args = reserve_private_args(unique_name, target);
    let mut cmd = Command::new(bin);
    cmd.args(&args).env("HOME", home);
    apply_api_endpoint(&mut cmd, api_endpoint);
    let output = cmd.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(token) = parse_share_token(&stdout).or_else(|| parse_share_token(&stderr)) {
        return Ok(token);
    }
    if let Some(name) = unique_name.filter(|s| !s.is_empty()) {
        if !output.status.success() {
            tracing::info!("zrok reserve reused existing unique-name");
        }
        return Ok(name.to_string());
    }
    if !output.status.success() {
        anyhow::bail!("zrok reserve private failed");
    }
    anyhow::bail!("zrok reserve private produced no share token")
}

pub async fn prepare_shares(
    config: &AgentConfig,
    paths: &SetupPaths,
    state: &mut ConvergeState,
    enable_token: Option<&str>,
    dry: bool,
) -> anyhow::Result<ShareTokens> {
    if !config.tunnel.enable {
        return Ok(ShareTokens::default());
    }
    let home = zrok_home(paths);
    std::fs::create_dir_all(&home)?;
    let configured = resolve_configured_tokens(config)?;
    let mut tokens = merge_tokens(configured, state);
    if dry {
        if tokens.ollama.is_empty() {
            tokens.ollama = "dry-ollama".into();
        }
        if tokens.agent.is_empty() {
            tokens.agent = derive_agent_share_token(&tokens.ollama);
        }
        persist_tokens(paths, state, &tokens)?;
        return Ok(tokens);
    }
    if !zrok_bin_present(&config.tunnel.zrok_bin) {
        anyhow::bail!(
            "zrok binary not found ({}); install zrok or set tunnel.zrok_bin",
            config.tunnel.zrok_bin
        );
    }
    maybe_enable(
        &config.tunnel.zrok_bin,
        &home,
        enable_token,
        configured_api_endpoint(config),
        false,
    )
    .await?;
    let ollama_hint = tokens.ollama.clone();
    tokens.ollama = reserve_share(
        &config.tunnel.zrok_bin,
        &home,
        empty_as_none(&ollama_hint),
        OLLAMA_LOOPBACK_URL,
        configured_api_endpoint(config),
        false,
    )
    .await?;
    let agent_hint = if tokens.agent.is_empty() {
        derive_agent_share_token(&tokens.ollama)
    } else {
        tokens.agent.clone()
    };
    tokens.agent = reserve_share(
        &config.tunnel.zrok_bin,
        &home,
        empty_as_none(&agent_hint),
        &agent_loopback_url(config.port),
        configured_api_endpoint(config),
        false,
    )
    .await?;
    persist_tokens(paths, state, &tokens)?;
    tracing::info!(
        ollama_share = %redact_secret(Some(&tokens.ollama)),
        ollama_share_id = %share_token_id(&tokens.ollama),
        agent_share = %redact_secret(Some(&tokens.agent)),
        agent_share_id = %share_token_id(&tokens.agent),
        find_url = %file_url(&find_path(paths)),
        "zrok private shares reserved"
    );
    Ok(tokens)
}

fn empty_as_none(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn persist_tokens(
    paths: &SetupPaths,
    state: &mut ConvergeState,
    tokens: &ShareTokens,
) -> anyhow::Result<()> {
    state.ollama_share_token = Some(tokens.ollama.clone());
    state.agent_share_token = Some(tokens.agent.clone());
    state.store(&paths.state)?;
    write_find_file(&find_path(paths), tokens)?;
    Ok(())
}

pub async fn wait_ollama_loopback_tags() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_secs(2))
        .build()?;
    let url = format!("{OLLAMA_LOOPBACK_URL}/api/tags");
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
    anyhow::bail!("ollama /api/tags did not become ready on 127.0.0.1:11434")
}

fn spawn_share(
    bin: &str,
    home: &Path,
    token: &str,
    api_endpoint: Option<&str>,
) -> anyhow::Result<Child> {
    let args = share_reserved_args(token);
    let mut cmd = Command::new(bin);
    cmd.args(&args)
        .env("HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_api_endpoint(&mut cmd, api_endpoint);
    let mut child = cmd.spawn()?;
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = parse_share_token(&line);
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
    }
    Ok(child)
}

/// Long-running sidecar: two `zrok share reserved` children.
pub async fn run_supervisor(config: AgentConfig, paths: SetupPaths) -> anyhow::Result<()> {
    if !config.tunnel.enable {
        anyhow::bail!("tunnel.enable is false");
    }
    let home = zrok_home(&paths);
    let state = ConvergeState::load(&paths.state);
    let tokens = merge_tokens(resolve_configured_tokens(&config)?, &state);
    if tokens.ollama.is_empty() || tokens.agent.is_empty() {
        anyhow::bail!("missing reserved share tokens; run `ollama-node-agent setup`");
    }
    tracing::info!(
        ollama_share = %redact_secret(Some(&tokens.ollama)),
        agent_share = %redact_secret(Some(&tokens.agent)),
        "starting zrok private shares"
    );
    loop {
        let mut ollama = spawn_share(
            &config.tunnel.zrok_bin,
            &home,
            &tokens.ollama,
            configured_api_endpoint(&config),
        )?;
        let mut agent = spawn_share(
            &config.tunnel.zrok_bin,
            &home,
            &tokens.agent,
            configured_api_endpoint(&config),
        )?;
        tokio::select! {
            status = ollama.wait() => {
                tracing::warn!(?status, share = "ollama", "zrok share exited");
                let _ = agent.kill().await;
            }
            status = agent.wait() => {
                tracing::warn!(?status, share = "agent", "zrok share exited");
                let _ = ollama.kill().await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_args_include_unique_name() {
        let args = reserve_private_args(Some("fleet-ollama"), OLLAMA_LOOPBACK_URL);
        assert_eq!(
            args,
            vec![
                "reserve",
                "private",
                "--headless",
                "--backend-mode",
                "proxy",
                "--unique-name",
                "fleet-ollama",
                OLLAMA_LOOPBACK_URL,
            ]
        );
    }

    #[test]
    fn share_reserved_args_are_headless() {
        let args = share_reserved_args("fleet-ollama");
        assert_eq!(
            args,
            vec!["share", "reserved", "--headless", "fleet-ollama"]
        );
    }

    #[test]
    fn parse_json_and_text_tokens() {
        assert_eq!(
            parse_share_token("{\"token\":\"abc123\"}\n"),
            Some("abc123".into())
        );
        assert_eq!(
            parse_share_token("your private share token is: xyz-token\n"),
            Some("xyz-token".into())
        );
        assert_eq!(
            parse_share_token("zrok access private secret-share\n"),
            Some("secret-share".into())
        );
        assert_eq!(parse_share_token("nothing useful"), None);
    }

    #[test]
    fn write_find_file_is_0600_and_has_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("find.json");
        let tokens = ShareTokens {
            ollama: "fleet-ollama".into(),
            agent: "fleet-ollama-agent".into(),
        };
        write_find_file(&path, &tokens).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("fleet-ollama"));
        assert!(!raw.contains("zrok-enable"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn configured_tokens_read_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "# comment\nfleet-from-file\n").unwrap();
        let mut cfg = crate::config::AgentConfig::default();
        cfg.tunnel.share_token_file = Some(path.display().to_string());
        let tokens = resolve_configured_tokens(&cfg).unwrap();
        assert_eq!(tokens.ollama, "fleet-from-file");
        assert_eq!(tokens.agent, "fleet-from-file-agent");
    }
}
