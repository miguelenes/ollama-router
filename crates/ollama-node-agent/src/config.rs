//! YAML config (`deny_unknown_fields`). Env overrides host/port/token only.

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
    Tailscale,
    Lan,
    All,
}

impl ListenMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Tailscale => "tailscale",
            Self::Lan => "lan",
            Self::All => "all",
        }
    }
}

/// `listen: 127.0.0.1` or `listen: tailscale`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum BindSpec {
    Mode(ListenMode),
    Address(String),
}

impl Default for BindSpec {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Self::Mode(ListenMode::Tailscale)
        } else {
            Self::Mode(ListenMode::Loopback)
        }
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
            listen: default_ollama_listen(),
            models_dir: None,
            extra_env: BTreeMap::new(),
        }
    }
}

fn default_ollama_listen() -> BindSpec {
    if cfg!(target_os = "linux") {
        BindSpec::Mode(ListenMode::Tailscale)
    } else {
        BindSpec::Mode(ListenMode::Loopback)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GpuSection {
    #[serde(default)]
    pub policy: GpuPolicy,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TailscaleSection {
    #[serde(default)]
    pub enable: bool,
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
}

fn default_register_token_env() -> String {
    "OLLAMA_ROUTER_ADMIN_TOKEN".into()
}

fn default_register_interval() -> u64 {
    30
}

impl Default for RegisterSection {
    fn default() -> Self {
        Self {
            url: None,
            token_env: default_register_token_env(),
            interval_seconds: default_register_interval(),
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
    pub tailscale: TailscaleSection,
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
            tailscale: TailscaleSection::default(),
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
    fn packaged_linux_config_is_tailscale() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/linux/config.yaml");
        let raw = std::fs::read_to_string(&path).unwrap();
        let cfg: AgentConfig = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(cfg.listen, BindSpec::Mode(ListenMode::Tailscale));
        assert_eq!(cfg.port, 11436);
    }
}
