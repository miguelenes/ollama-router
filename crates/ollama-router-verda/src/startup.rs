//! Verda first-boot startup script (agent install + setup). Never logs secrets.

use ollama_router_core::config::VerdaConfig;

const BOOTSTRAP_EOF: &str = "OLLAMA_ROUTER_BOOTSTRAP_EOF";
const INSTALLER: &str = include_str!("agent_init.sh");

pub struct StartupScriptParams<'a> {
    pub enroll_url: Option<&'a str>,
    pub zrok_enable_token: Option<&'a str>,
    pub zrok_api_endpoint: Option<&'a str>,
    pub enroll_token: Option<&'a str>,
    pub enroll_token_env: &'a str,
    pub package_url: Option<&'a str>,
    pub deb_amd64: &'a str,
    pub deb_arm64: &'a str,
    pub tar_amd64: &'a str,
    pub tar_arm64: &'a str,
}

impl std::fmt::Debug for StartupScriptParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupScriptParams")
            .field("enroll_url", &self.enroll_url)
            .field(
                "zrok_enable_token",
                &self.zrok_enable_token.map(|_| "REDACTED"),
            )
            .field("zrok_api_endpoint", &self.zrok_api_endpoint)
            .field("enroll_token", &self.enroll_token.map(|_| "REDACTED"))
            .field("enroll_token_env", &self.enroll_token_env)
            .field("package_url", &self.package_url)
            .field("deb_amd64", &self.deb_amd64)
            .field("deb_arm64", &self.deb_arm64)
            .field("tar_amd64", &self.tar_amd64)
            .field("tar_arm64", &self.tar_arm64)
            .finish()
    }
}

pub fn github_asset_url(repo: &str, version: &str, filename: &str) -> String {
    let ver = version.trim().trim_start_matches('v');
    format!("https://github.com/{repo}/releases/download/v{ver}/{filename}")
}

pub fn default_package_urls(config: &VerdaConfig) -> (String, String, String, String) {
    let repo = config.agent_github_repo.trim();
    let version = config
        .agent_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"));
    let ver = version.trim_start_matches('v');
    (
        github_asset_url(repo, ver, &format!("ollama-node-agent_{ver}_amd64.deb")),
        github_asset_url(repo, ver, &format!("ollama-node-agent_{ver}_arm64.deb")),
        github_asset_url(repo, ver, "ollama-node-agent-linux-amd64.tar.gz"),
        github_asset_url(repo, ver, "ollama-node-agent-linux-arm64.tar.gz"),
    )
}

fn reject_injection(value: &str, field: &str) -> Result<(), String> {
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(format!("{field} must be a single line"));
    }
    if value.contains(BOOTSTRAP_EOF) {
        return Err(format!("{field} contains reserved delimiter"));
    }
    Ok(())
}

fn push_env_line(out: &mut String, key: &str, value: &str) -> Result<(), String> {
    reject_injection(key, "env key")?;
    reject_injection(value, key)?;
    out.push_str(key);
    out.push('=');
    out.push_str(value);
    out.push('\n');
    Ok(())
}

/// Build the catalog script body. Secrets are written to a 0600 env file only.
pub fn agent_init_script(params: &StartupScriptParams<'_>) -> Result<String, String> {
    reject_injection(params.enroll_token_env, "enroll_token_env")?;
    reject_injection(params.deb_amd64, "deb_amd64")?;
    reject_injection(params.deb_arm64, "deb_arm64")?;
    reject_injection(params.tar_amd64, "tar_amd64")?;
    reject_injection(params.tar_arm64, "tar_arm64")?;
    if let Some(url) = params.enroll_url {
        reject_injection(url, "enroll_url")?;
        if url.contains('"') {
            return Err("enroll_url must not contain quotes".into());
        }
    }
    if let Some(url) = params.zrok_api_endpoint {
        reject_injection(url, "zrok_api_endpoint")?;
        if url.contains('"') {
            return Err("zrok_api_endpoint must not contain quotes".into());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("zrok_api_endpoint must be http(s)".into());
        }
    }
    if let Some(url) = params.package_url {
        reject_injection(url, "package_url")?;
    }

    let mut body = String::from("#!/bin/bash\n");
    body.push_str("set -euo pipefail\n");
    body.push_str("set +o xtrace\n");
    body.push_str("umask 077\n");
    body.push_str("mkdir -p /run/ollama-router /var/lib/ollama-node-agent\n");
    body.push_str("cat > /run/ollama-router/bootstrap.env <<'");
    body.push_str(BOOTSTRAP_EOF);
    body.push_str("'\n");
    push_env_line(&mut body, "ENROLL_TOKEN_ENV", params.enroll_token_env)?;
    push_env_line(&mut body, "AGENT_DEB_AMD64", params.deb_amd64)?;
    push_env_line(&mut body, "AGENT_DEB_ARM64", params.deb_arm64)?;
    push_env_line(&mut body, "AGENT_TAR_AMD64", params.tar_amd64)?;
    push_env_line(&mut body, "AGENT_TAR_ARM64", params.tar_arm64)?;
    if let Some(url) = params.enroll_url.map(str::trim).filter(|s| !s.is_empty()) {
        push_env_line(&mut body, "ENROLL_URL", url)?;
    }
    if let Some(url) = params.package_url.map(str::trim).filter(|s| !s.is_empty()) {
        push_env_line(&mut body, "AGENT_PACKAGE_URL", url)?;
    }
    if let Some(token) = params
        .zrok_enable_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        push_env_line(&mut body, "ZROK_ENABLE_TOKEN", token)?;
    }
    if let Some(endpoint) = params
        .zrok_api_endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        push_env_line(&mut body, "ZROK_API_ENDPOINT", endpoint)?;
    }
    if let Some(token) = params.enroll_token.map(str::trim).filter(|s| !s.is_empty()) {
        push_env_line(&mut body, "OLLAMA_ROUTER_ADMIN_TOKEN", token)?;
    }
    body.push_str(BOOTSTRAP_EOF);
    body.push('\n');
    body.push_str("chmod 600 /run/ollama-router/bootstrap.env\n");
    body.push('\n');
    body.push_str(INSTALLER);
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StartupScriptParams<'static> {
        StartupScriptParams {
            enroll_url: Some("http://router.example:11435"),
            zrok_enable_token: Some("enable-secret"),
            zrok_api_endpoint: Some("http://127.0.0.1:18080"),
            enroll_token: Some("admin-secret"),
            enroll_token_env: "OLLAMA_ROUTER_ADMIN_TOKEN",
            package_url: None,
            deb_amd64: "https://example.invalid/amd64.deb",
            deb_arm64: "https://example.invalid/arm64.deb",
            tar_amd64: "https://example.invalid/amd64.tar.gz",
            tar_arm64: "https://example.invalid/arm64.tar.gz",
        }
    }

    #[test]
    fn installer_is_idempotent_and_quiet() {
        let script = agent_init_script(&sample()).expect("script");
        assert!(script.contains("ollama-node-agent"));
        assert!(script.contains("setup"));
        assert!(script.contains("command -v ollama-node-agent"));
        assert!(script.contains("set +o xtrace"));
        assert!(!script.contains("tailscale"));
        assert!(!script.contains("TS_AUTHKEY"));
        assert!(!script.contains("127.0.0.1:11434"));
        assert!(!script.contains("set -x"));
        assert!(!script.contains("echo \"$ZROK"));
        assert!(!script.contains("echo $ZROK"));
        assert!(!script.contains("echo \"$OLLAMA_ROUTER_ADMIN_TOKEN"));
        assert!(script.contains("enable-secret"));
        assert!(script.contains("ZROK_ENABLE_TOKEN=enable-secret"));
        assert!(script.contains("ZROK_API_ENDPOINT=http://127.0.0.1:18080"));
        assert!(script.contains("api_endpoint:"));
        assert!(!script.contains("echo \"$ZROK_API"));
    }

    #[test]
    fn startup_params_debug_redacts_tokens() {
        let dbg = format!("{:?}", sample());
        assert!(!dbg.contains("enable-secret"), "{dbg}");
        assert!(!dbg.contains("admin-secret"), "{dbg}");
        assert!(dbg.contains("REDACTED"), "{dbg}");
    }

    #[test]
    fn rejects_newline_secrets() {
        let mut params = sample();
        params.zrok_enable_token = Some("one\ntwo");
        assert!(agent_init_script(&params).is_err());
    }
}
