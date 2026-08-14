//! Marker file so converge is a state machine, not `if command -v`.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const STATE_SCHEMA: u32 = 4;

pub const SUPERVISOR_SYSTEMD: &str = "systemd";
pub const SUPERVISOR_MANUAL: &str = "manual";
pub const SUPERVISOR_SCM: &str = "scm";
pub const SUPERVISOR_LAUNCHD: &str = "launchd";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ConvergeState {
    pub schema: u32,
    pub ollama_installed: bool,
    pub ollama_version: Option<String>,
    pub unit_written: bool,
    pub tunnel_unit_written: bool,
    pub last_converge: Option<String>,
    pub listen_mode: Option<String>,
    pub bind: Option<String>,
    /// How the agent is supposed to run: `systemd` | `manual` | `scm` | `launchd`.
    pub supervisor: Option<String>,
    /// Reserved zrok unique-name for Ollama. Never log.
    pub ollama_share_token: Option<String>,
    /// Reserved zrok unique-name for the agent. Never log.
    pub agent_share_token: Option<String>,
    /// Router enroll URL (full path or origin). Not a secret.
    pub enroll_url: Option<String>,
    /// Name of the env var holding the admin bearer. Never the token itself.
    pub enroll_token_env: Option<String>,
}

impl ConvergeState {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self {
                schema: STATE_SCHEMA,
                ..Self::default()
            };
        };
        serde_json::from_str(&raw).unwrap_or(Self {
            schema: STATE_SCHEMA,
            ..Self::default()
        })
    }

    pub fn store(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn share_present(&self) -> bool {
        self.ollama_share_token
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && self
                .agent_share_token
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
    }
}

fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Persist enroll URL + token *env name* only. Never writes the bearer.
pub fn apply_enroll_flags(
    state: &mut ConvergeState,
    enroll_url: &str,
    token_env: Option<&str>,
) -> anyhow::Result<()> {
    let url = enroll_url.trim();
    if url.is_empty() {
        anyhow::bail!("--enroll-url must be non-empty");
    }
    let env_name = token_env
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("OLLAMA_ROUTER_ADMIN_TOKEN");
    if !is_env_name(env_name) {
        anyhow::bail!("--enroll-token-env must name an environment variable");
    }
    state.enroll_url = Some(url.to_string());
    state.enroll_token_env = Some(env_name.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_json_missing_supervisor() {
        let raw = r#"{"schema":1,"ollama_installed":true,"unit_written":true}"#;
        let state: ConvergeState = serde_json::from_str(raw).unwrap();
        assert_eq!(state.supervisor, None);
        assert!(state.ollama_installed);
        assert!(state.unit_written);
        assert!(!state.share_present());
        assert!(!state.tunnel_unit_written);
        assert!(state.enroll_url.is_none());
        assert!(state.enroll_token_env.is_none());
    }

    #[test]
    fn apply_enroll_flags_stores_env_name_not_token() {
        let mut state = ConvergeState {
            schema: STATE_SCHEMA,
            ..ConvergeState::default()
        };
        apply_enroll_flags(
            &mut state,
            "http://router:11435/router/v1/nodes/enroll",
            Some("OLLAMA_ROUTER_ADMIN_TOKEN"),
        )
        .unwrap();
        assert_eq!(
            state.enroll_url.as_deref(),
            Some("http://router:11435/router/v1/nodes/enroll")
        );
        assert_eq!(
            state.enroll_token_env.as_deref(),
            Some("OLLAMA_ROUTER_ADMIN_TOKEN")
        );
        assert!(apply_enroll_flags(&mut state, "http://x", Some("not a token")).is_err());
        assert!(apply_enroll_flags(&mut state, "http://x", Some("secret-value")).is_err());
    }
}
