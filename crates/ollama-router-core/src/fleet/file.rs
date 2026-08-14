//! Declarative permanent fleet membership (`OLLAMA_ROUTER_FLEET`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::error::ConfigError;
use crate::config::models::{reject_duplicate_node_ids, Capacity, NodeConfig};
use crate::fleet::ids::NodeId;

/// Default GitOps inventory path.
pub const DEFAULT_FLEET_PATH: &str = "/etc/ollama-router/fleet.yaml";

const ENV_FLEET: &str = "OLLAMA_ROUTER_FLEET";

/// Resolve fleet path from env. `explicit` is true when `OLLAMA_ROUTER_FLEET` is set.
pub fn fleet_path_from_env(env: &impl crate::config::EnvSource) -> (PathBuf, bool) {
    match env.var(ENV_FLEET) {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                (PathBuf::from(DEFAULT_FLEET_PATH), false)
            } else {
                (PathBuf::from(trimmed), true)
            }
        }
        None => (PathBuf::from(DEFAULT_FLEET_PATH), false),
    }
}

/// Load permanent nodes from a fleet YAML file.
///
/// * `fail_if_missing` — explicit `OLLAMA_ROUTER_FLEET` path: missing file is an error.
/// * Otherwise a missing default path yields an empty permanent list.
pub fn load_fleet_nodes(
    path: &Path,
    fail_if_missing: bool,
) -> Result<Vec<NodeConfig>, ConfigError> {
    if !path.is_file() {
        if fail_if_missing {
            return Err(ConfigError::invalid(format!(
                "fleet file not found: {}",
                path.display()
            )));
        }
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)?;
    parse_fleet_yaml(&text, &path.display().to_string())
}

/// Parse fleet YAML text into [`NodeConfig`] values (not yet URL-hydrated).
pub fn parse_fleet_yaml(source: &str, origin: &str) -> Result<Vec<NodeConfig>, ConfigError> {
    if source.trim().is_empty() {
        return Err(ConfigError::invalid(format!(
            "{origin}: fleet file must declare version: 1"
        )));
    }
    let doc: FleetDocument = serde_yaml::from_str(source)
        .map_err(|e| ConfigError::InvalidYaml(format!("{origin}: {e}")))?;
    if doc.version != 1 {
        return Err(ConfigError::invalid(format!(
            "{origin}: unsupported fleet version {} (expected 1)",
            doc.version
        )));
    }
    let mut nodes = Vec::with_capacity(doc.nodes.len());
    for node in doc.nodes {
        nodes.push(node.into_config()?);
    }
    reject_duplicate_node_ids(&nodes)?;
    Ok(nodes)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetDocument {
    version: u32,
    #[serde(default)]
    nodes: Vec<FleetNode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetNode {
    id: NodeId,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    capacity: Capacity,
    #[serde(default)]
    capacity_url: Option<String>,
    #[serde(default)]
    max_inflight: Option<u32>,
}

impl FleetNode {
    fn into_config(self) -> Result<NodeConfig, ConfigError> {
        let mut node = NodeConfig {
            id: self.id,
            url: self.url,
            capacity_url: self.capacity_url,
            labels: self.labels,
            static_capacity: self.capacity,
            max_inflight: self.max_inflight,
        };
        node.normalize_and_validate()?;
        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn origin() -> &'static str {
        "test-fleet.yaml"
    }

    #[test]
    fn parses_nested_capacity_labels() {
        let yaml = r#"
version: 1
nodes:
  - id: desk
    url: http://desk:11434
    labels: [gpu, always-on]
    capacity: { vram_gb: 8 }
    max_inflight: 2
  - id: laptop
    url: http://laptop:11434
    labels: [cpu]
  - id: colo
    url: http://10.0.0.5:11434
    labels: [gpu]
"#;
        let nodes = parse_fleet_yaml(yaml, origin()).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].id.as_str(), "desk");
        assert_eq!(nodes[0].static_capacity.vram_gb, Some(8.0));
        assert_eq!(nodes[0].max_inflight, Some(2));
        assert_eq!(nodes[0].labels, ["gpu", "always-on"]);
        assert_eq!(nodes[1].labels, ["cpu"]);
        assert_eq!(nodes[2].url.as_deref(), Some("http://10.0.0.5:11434"));
    }

    #[test]
    fn ssh_and_provision_fields_rejected() {
        let ssh = r#"
version: 1
nodes:
  - id: colo
    url: http://10.0.0.5:11434
    ssh:
      host: 203.0.113.10
"#;
        assert!(parse_fleet_yaml(ssh, origin()).is_err());
        let provision = r#"
version: 1
nodes:
  - id: a
    url: http://a:11434
    provision: true
"#;
        assert!(parse_fleet_yaml(provision, origin()).is_err());
    }

    #[test]
    fn duplicate_ids_error() {
        let yaml = r#"
version: 1
nodes:
  - id: same
    url: http://a:11434
  - id: same
    url: http://b:11434
"#;
        let err = parse_fleet_yaml(yaml, origin()).unwrap_err();
        assert!(err.to_string().contains("unique"));
    }

    #[test]
    fn unknown_version_error() {
        let err = parse_fleet_yaml("version: 2\nnodes: []\n", origin()).unwrap_err();
        assert!(err.to_string().contains("unsupported fleet version"));
    }

    #[test]
    fn missing_version_error() {
        assert!(parse_fleet_yaml("nodes: []\n", origin()).is_err());
    }

    #[test]
    fn empty_nodes_ok() {
        let nodes = parse_fleet_yaml("version: 1\nnodes: []\n", origin()).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn url_required_without_url_is_invalid() {
        let yaml = r#"
version: 1
nodes:
  - id: colo
    labels: [gpu]
"#;
        let err = parse_fleet_yaml(yaml, origin()).unwrap_err();
        assert!(err.to_string().contains("needs a routing url"));
    }

    #[test]
    fn nan_vram_is_invalid() {
        let yaml = r#"
version: 1
nodes:
  - id: desk
    url: http://desk:11434
    capacity:
      vram_gb: .nan
"#;
        let err = parse_fleet_yaml(yaml, origin()).unwrap_err();
        assert!(err.to_string().contains("finite"));
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(parse_fleet_yaml(
            "version: 1\nnodes:\n  - id: a\n    url: http://a:11434\n    thunder: true\n",
            origin()
        )
        .is_err());
    }

    #[test]
    fn missing_explicit_path_errors() {
        let path = PathBuf::from("/no/such/ollama-router-fleet.yaml");
        let err = load_fleet_nodes(&path, true).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn missing_default_path_is_empty() {
        let path = PathBuf::from("/no/such/ollama-router-fleet.yaml");
        let nodes = load_fleet_nodes(&path, false).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn load_from_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.yaml");
        fs::write(
            &path,
            "version: 1\nnodes:\n  - id: n\n    url: http://n:11434\n    capacity_url: http://n:11436/v1/capacity\n",
        )
        .unwrap();
        let nodes = load_fleet_nodes(&path, true).unwrap();
        assert_eq!(nodes[0].id.as_str(), "n");
        assert_eq!(
            nodes[0].capacity_url.as_deref(),
            Some("http://n:11436/v1/capacity")
        );
    }
}
