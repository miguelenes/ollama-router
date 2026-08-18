//! RunPod dockerStartCmd bootstrap (agent install + setup). Secrets stay in env, never logged.

use std::collections::BTreeMap;

use ollama_router_core::config::RunpodConfig;

use crate::types::CreatePodRequest;

const INSTALLER: &str = include_str!("agent_init.sh");
const BOOTSTRAP_EOF: &str = "OLLAMA_ROUTER_BOOTSTRAP_EOF";

pub struct BootstrapParams<'a> {
    pub enroll_url: Option<&'a str>,
    pub zrok_api_endpoint: Option<&'a str>,
    pub enroll_token_env: &'a str,
    pub package_url: Option<&'a str>,
    pub deb_amd64: &'a str,
    pub deb_arm64: &'a str,
    pub tar_amd64: &'a str,
    pub tar_arm64: &'a str,
    /// Secret env values keyed by configured env var names. Never logged.
    pub secret_env: BTreeMap<String, String>,
}

impl std::fmt::Debug for BootstrapParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapParams")
            .field("enroll_url", &self.enroll_url)
            .field("zrok_api_endpoint", &self.zrok_api_endpoint)
            .field("enroll_token_env", &self.enroll_token_env)
            .field("package_url", &self.package_url)
            .field("deb_amd64", &self.deb_amd64)
            .field("deb_arm64", &self.deb_arm64)
            .field("tar_amd64", &self.tar_amd64)
            .field("tar_arm64", &self.tar_arm64)
            .field(
                "secret_env",
                &self
                    .secret_env
                    .keys()
                    .map(|k| (k.as_str(), "REDACTED"))
                    .collect::<BTreeMap<_, _>>(),
            )
            .finish()
    }
}

pub fn github_asset_url(repo: &str, version: &str, filename: &str) -> String {
    let ver = version.trim().trim_start_matches('v');
    format!("https://github.com/{repo}/releases/download/v{ver}/{filename}")
}

pub fn default_package_urls(config: &RunpodConfig) -> (String, String, String, String) {
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

/// Non-secret bootstrap script. Secrets arrive via container `env`, not this string.
pub fn agent_bootstrap_script(params: &BootstrapParams<'_>) -> Result<String, String> {
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
    // Materialize non-secret knobs; secrets already present as container env.
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
    if let Some(endpoint) = params
        .zrok_api_endpoint
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        push_env_line(&mut body, "ZROK_API_ENDPOINT", endpoint)?;
    }
    body.push_str(BOOTSTRAP_EOF);
    body.push('\n');
    body.push_str("chmod 600 /run/ollama-router/bootstrap.env\n");
    body.push_str("OLLAMA_HOST=127.0.0.1 ollama serve &\n");
    // Merge container secret env into bootstrap.env without echoing values.
    body.push_str(
        r#"
set -a
# shellcheck disable=SC1091
. /run/ollama-router/bootstrap.env
set +a
if [[ -n "${ZROK_ENABLE_TOKEN:-}" ]]; then
  printf 'ZROK_ENABLE_TOKEN=%s\n' "${ZROK_ENABLE_TOKEN}" >>/run/ollama-router/bootstrap.env
fi
if [[ -n "${OLLAMA_ROUTER_ADMIN_TOKEN:-}" ]]; then
  printf 'OLLAMA_ROUTER_ADMIN_TOKEN=%s\n' "${OLLAMA_ROUTER_ADMIN_TOKEN}" >>/run/ollama-router/bootstrap.env
fi
"#,
    );
    body.push_str(INSTALLER);
    Ok(body)
}

pub fn build_create_request(
    name: &str,
    interruptible: bool,
    gpu_type_ids: Vec<String>,
    data_center_ids: Option<Vec<String>>,
    config: &RunpodConfig,
    params: &BootstrapParams<'_>,
) -> Result<CreatePodRequest, String> {
    let script = agent_bootstrap_script(params)?;
    let template_id = config
        .template_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(CreatePodRequest {
        name: name.to_string(),
        image_name: config.image.clone(),
        interruptible,
        cloud_type: config.cloud_type.clone(),
        gpu_type_ids,
        gpu_type_priority: "custom".into(),
        docker_entrypoint: vec!["/bin/bash".into()],
        docker_start_cmd: vec!["-lc".into(), script],
        env: params.secret_env.clone(),
        container_disk_in_gb: config.container_disk_gb,
        volume_in_gb: 0,
        ports: vec![],
        template_id,
        data_center_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BootstrapParams<'static> {
        let mut secret_env = BTreeMap::new();
        secret_env.insert("ZROK_ENABLE_TOKEN".into(), "enable-secret".into());
        secret_env.insert("OLLAMA_ROUTER_ADMIN_TOKEN".into(), "admin-secret".into());
        BootstrapParams {
            enroll_url: Some("http://router.example:11435"),
            zrok_api_endpoint: Some("http://127.0.0.1:18080"),
            enroll_token_env: "OLLAMA_ROUTER_ADMIN_TOKEN",
            package_url: None,
            deb_amd64: "https://example.invalid/amd64.deb",
            deb_arm64: "https://example.invalid/arm64.deb",
            tar_amd64: "https://example.invalid/amd64.tar.gz",
            tar_arm64: "https://example.invalid/arm64.tar.gz",
            secret_env,
        }
    }

    #[test]
    fn docker_start_cmd_has_no_token_material() {
        let params = sample();
        let config = RunpodConfig::default();
        let req = build_create_request(
            "or-rp-test",
            true,
            vec!["NVIDIA L4".into()],
            None,
            &config,
            &params,
        )
        .expect("request");
        assert_eq!(req.docker_entrypoint, vec!["/bin/bash"]);
        assert_eq!(req.docker_start_cmd[0], "-lc");
        let script = &req.docker_start_cmd[1];
        assert!(!script.contains("enable-secret"), "{script}");
        assert!(!script.contains("admin-secret"), "{script}");
        assert!(script.contains("origin: runpod"));
        assert!(script.contains("OLLAMA_HOST=127.0.0.1 ollama serve"));
        assert_eq!(req.image_name, "ollama/ollama:latest");
        assert_eq!(req.volume_in_gb, 0);
        assert!(req.ports.is_empty());
        assert!(req.template_id.is_none());
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("enable-secret"), "{dbg}");
        assert!(!dbg.contains("admin-secret"), "{dbg}");
        assert!(dbg.contains("REDACTED"), "{dbg}");
        let params_dbg = format!("{params:?}");
        assert!(!params_dbg.contains("enable-secret"), "{params_dbg}");
    }

    #[test]
    fn agent_init_package_failure_sleeps_instead_of_exit() {
        assert!(INSTALLER.contains("exec sleep infinity"));
        assert!(INSTALLER.contains("reason_code=agent_package_unavailable"));
        assert!(!INSTALLER.contains("exit 1"));
    }

    #[test]
    fn template_id_is_honored_when_configured() {
        let params = sample();
        let config = RunpodConfig {
            template_id: Some("my-runpod-template".into()),
            ..RunpodConfig::default()
        };
        let req = build_create_request(
            "or-rp-test",
            true,
            vec!["NVIDIA L4".into()],
            None,
            &config,
            &params,
        )
        .expect("request");
        assert_eq!(req.template_id.as_deref(), Some("my-runpod-template"));
        assert_eq!(
            req.to_json()["templateId"],
            serde_json::json!("my-runpod-template")
        );
    }
}
