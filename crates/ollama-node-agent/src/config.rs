//! YAML config (`deny_unknown_fields`). Env overrides host/port/token/`ZROK_API_ENDPOINT`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read config {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse config {path}: {source}")]
    Yaml {
        path: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("{0}")]
    Message(String),
}

/// How to pick a bind address.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenMode {
    #[default]
    Loopback,
    Lan,
    All,
}

impl ListenMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Lan => "lan",
            Self::All => "all",
        }
    }
}

/// `listen: 127.0.0.1` or `listen: loopback`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum BindSpec {
    Mode(ListenMode),
    Address(String),
}

impl Default for BindSpec {
    fn default() -> Self {
        Self::Mode(ListenMode::Loopback)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuPolicy {
    #[default]
    Auto,
    Cpu,
    Cuda,
    Rocm,
    Metal,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OllamaSection {
    #[serde(default)]
    pub listen: BindSpec,
    #[serde(default)]
    pub models_dir: Option<String>,
    #[serde(default)]
    pub extra_env: BTreeMap<String, String>,
}

impl Default for OllamaSection {
    fn default() -> Self {
        Self {
            listen: BindSpec::Mode(ListenMode::Loopback),
            models_dir: None,
            extra_env: BTreeMap::new(),
        }
    }
}

/// Self-hosted zrok **private** shares. Never put an enable token in YAML.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TunnelSection {
    #[serde(default = "default_tunnel_enable")]
    pub enable: bool,
    #[serde(default = "default_zrok_bin")]
    pub zrok_bin: String,
    /// Reserved unique-name for the Ollama share (`127.0.0.1:11434`).
    #[serde(default)]
    pub share_token: Option<String>,
    /// Reserved unique-name for the agent share (`127.0.0.1:11436`).
    #[serde(default)]
    pub agent_share_token: Option<String>,
    /// One-line file with the Ollama reserved share token (0600).
    #[serde(default)]
    pub share_token_file: Option<String>,
    /// Path to a zrok enable token. Setup-only; never logged or copied into serve.
    #[serde(default)]
    pub enable_token_file: Option<String>,
    /// Self-hosted controller API (`ZROK_API_ENDPOINT`). Empty uses process env.
    #[serde(default)]
    pub api_endpoint: Option<String>,
}

fn default_tunnel_enable() -> bool {
    true
}

fn default_zrok_bin() -> String {
    "zrok".into()
}

impl Default for TunnelSection {
    fn default() -> Self {
        Self {
            enable: default_tunnel_enable(),
            zrok_bin: default_zrok_bin(),
            share_token: None,
            agent_share_token: None,
            share_token_file: None,
            enable_token_file: None,
            api_endpoint: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GpuSection {
    #[serde(default)]
    pub policy: GpuPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegisterSection {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_register_token_env")]
    pub token_env: String,
    #[serde(default = "default_register_interval")]
    pub interval_seconds: u64,
    /// Fleet / Verda / adopt node id. Defaults to hostname at heartbeat time.
    #[serde(default)]
    pub node_id: Option<String>,
    /// `fleet` | `verda` | `adopt`. Enroll hydrates reachability only.
    #[serde(default = "default_register_origin")]
    pub origin: String,
}

fn default_register_token_env() -> String {
    "OLLAMA_ROUTER_ADMIN_TOKEN".into()
}

fn default_register_interval() -> u64 {
    30
}

fn default_register_origin() -> String {
    "adopt".into()
}

impl Default for RegisterSection {
    fn default() -> Self {
        Self {
            url: None,
            token_env: default_register_token_env(),
            interval_seconds: default_register_interval(),
            node_id: None,
            origin: default_register_origin(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    #[serde(default)]
    pub listen: BindSpec,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub ollama: OllamaSection,
    #[serde(default)]
    pub gpu: GpuSection,
    #[serde(default)]
    pub tunnel: TunnelSection,
    #[serde(default)]
    pub register: RegisterSection,
}

fn default_port() -> u16 {
    11436
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            listen: BindSpec::default(),
            port: default_port(),
            token: None,
            ollama: OllamaSection::default(),
            gpu: GpuSection::default(),
            tunnel: TunnelSection::default(),
            register: RegisterSection::default(),
        }
    }
}

impl AgentConfig {
    pub fn default_path() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\ProgramData\ollama-node-agent\config.yaml")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/Library/Application Support/ollama-node-agent/config.yaml")
        } else {
            PathBuf::from("/etc/ollama-node-agent/config.yaml")
        }
    }

    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(Self::default_path);
        if !path.exists() {
            let mut cfg = Self::default();
            cfg.apply_env();
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let mut cfg: Self = serde_yaml::from_str(&raw).map_err(|source| ConfigError::Yaml {
            path: path.display().to_string(),
            source,
        })?;
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(host) = std::env::var("OLLAMA_NODE_AGENT_HOST") {
            let host = host.trim();
            if !host.is_empty() {
                self.listen = BindSpec::Address(host.to_string());
            }
        }
        if let Ok(port) = std::env::var("OLLAMA_NODE_AGENT_PORT") {
            if let Ok(p) = port.trim().parse() {
                self.port = p;
            }
        }
        if let Ok(token) = std::env::var("OLLAMA_NODE_AGENT_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                self.token = Some(token.to_string());
            }
        }
        if let Ok(bin) = std::env::var("OLLAMA_NODE_AGENT_ZROK_BIN") {
            let bin = bin.trim();
            if !bin.is_empty() {
                self.tunnel.zrok_bin = bin.to_string();
            }
        }
        if let Ok(token) = std::env::var("OLLAMA_NODE_AGENT_SHARE_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                self.tunnel.share_token = Some(token.to_string());
            }
        }
        if let Ok(token) = std::env::var("OLLAMA_NODE_AGENT_AGENT_SHARE_TOKEN") {
            let token = token.trim();
            if !token.is_empty() {
                self.tunnel.agent_share_token = Some(token.to_string());
            }
        }
        if let Ok(endpoint) = std::env::var("ZROK_API_ENDPOINT") {
            let endpoint = endpoint.trim();
            if !endpoint.is_empty() {
                self.tunnel.api_endpoint = Some(endpoint.to_string());
            }
        }
    }

    pub fn bearer_token(&self) -> Option<&str> {
        self.token.as_deref().filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_yaml_field_is_denied() {
        let err = serde_yaml::from_str::<AgentConfig>("foo: 1").unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn listen_mode_parses() {
        let cfg: AgentConfig = serde_yaml::from_str("listen: loopback\nport: 11436\n").unwrap();
        assert_eq!(cfg.listen, BindSpec::Mode(ListenMode::Loopback));
        assert_eq!(cfg.port, 11436);
    }

    #[test]
    fn listen_tailscale_string_is_address_not_mode() {
        let cfg: AgentConfig = serde_yaml::from_str("listen: tailscale\n").unwrap();
        assert_eq!(cfg.listen, BindSpec::Address("tailscale".into()));
    }

    fn assert_packaged_loopback(rel: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
        let raw = std::fs::read_to_string(&path).unwrap();
        let cfg: AgentConfig = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(cfg.listen, BindSpec::Mode(ListenMode::Loopback));
        assert_eq!(cfg.ollama.listen, BindSpec::Mode(ListenMode::Loopback));
        assert_eq!(cfg.port, 11436);
        assert!(cfg.tunnel.enable);
        assert_eq!(cfg.tunnel.zrok_bin, "zrok");
    }

    #[test]
    fn tunnel_api_endpoint_parses() {
        let cfg: AgentConfig =
            serde_yaml::from_str("tunnel:\n  api_endpoint: http://127.0.0.1:18080\n").unwrap();
        assert_eq!(
            cfg.tunnel.api_endpoint.as_deref(),
            Some("http://127.0.0.1:18080")
        );
    }

    #[test]
    fn packaged_linux_config_is_loopback_tunnel() {
        assert_packaged_loopback("packaging/linux/config.yaml");
    }

    #[test]
    fn packaged_windows_config_is_loopback_tunnel() {
        assert_packaged_loopback("packaging/windows/config.yaml");
    }

    #[test]
    fn packaged_macos_config_is_loopback_tunnel() {
        assert_packaged_loopback("packaging/macos/config.yaml");
    }

    #[test]
    fn docker_and_local_configs_disable_tunnel() {
        for rel in ["config.docker.yaml", "../../deploy/agent.local.yaml"] {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
            let raw = std::fs::read_to_string(&path).unwrap();
            let cfg: AgentConfig = serde_yaml::from_str(&raw).unwrap();
            assert_eq!(cfg.listen, BindSpec::Mode(ListenMode::Loopback));
            assert_eq!(cfg.ollama.listen, BindSpec::Mode(ListenMode::Loopback));
            assert!(!cfg.tunnel.enable);
        }
    }
}
