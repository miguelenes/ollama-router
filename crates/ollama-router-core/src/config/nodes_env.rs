//! Compact `OLLAMA_ROUTER_NODES` override (tests/dev).

use crate::fleet::ids::NodeId;

use super::error::ConfigError;
use super::models::{Capacity, NodeConfig};

/// Parse `id|url[|k=v,...];id2|url2` compact inventory.
pub fn parse_nodes_env(value: &str) -> Result<Vec<NodeConfig>, ConfigError> {
    let mut nodes = Vec::new();
    for chunk in value.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let parts: Vec<&str> = chunk.split('|').collect();
        if parts.len() < 2 {
            return Err(ConfigError::invalid(format!(
                "node entry must be id|url[|spec]: {chunk:?}"
            )));
        }
        let node_id = NodeId::parse(parts[0].trim())
            .map_err(|e| ConfigError::invalid(format!("invalid node entry {chunk:?}: {e}")))?;
        let url = parts[1].trim().to_string();
        let spec = if parts.len() > 2 { parts[2].trim() } else { "" };

        let mut capacity = Capacity::default();
        let mut labels = Vec::new();
        let mut capacity_url = None;

        if !spec.is_empty() {
            for pair in spec.split(',') {
                let pair = pair.trim();
                if pair.is_empty() {
                    continue;
                }
                let Some((key, val)) = pair.split_once('=') else {
                    return Err(ConfigError::invalid(format!(
                        "spec entry must be key=value: {pair:?}"
                    )));
                };
                let key = key.trim();
                let val = val.trim();
                match key {
                    "labels" => {
                        labels = val
                            .split(':')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                    }
                    "vram" => {
                        capacity.vram_gb = Some(parse_float(key, val)?);
                    }
                    "ram" => {
                        capacity.ram_gb = Some(parse_float(key, val)?);
                    }
                    "gpus" => {
                        capacity.gpus = Some(parse_u32(key, val)?);
                    }
                    "cores" => {
                        capacity.cpu_cores = Some(parse_u32(key, val)?);
                    }
                    "capacity_url" => {
                        capacity_url = Some(val.to_string());
                    }
                    other => {
                        return Err(ConfigError::invalid(format!("unknown spec key: {other:?}")));
                    }
                }
            }
        }

        let mut node = NodeConfig {
            id: node_id,
            url: Some(url),
            capacity_url,
            labels,
            static_capacity: capacity,
            max_inflight: None,
            ssh: None,
            provision: None,
        };
        node.normalize_and_validate()
            .map_err(|e| ConfigError::invalid(format!("invalid node entry {chunk:?}: {e}")))?;
        nodes.push(node);
    }
    super::models::reject_duplicate_node_ids(&nodes)?;
    Ok(nodes)
}

fn parse_float(key: &str, val: &str) -> Result<f64, ConfigError> {
    val.parse::<f64>()
        .map_err(|_| ConfigError::invalid(format!("{key} must be a number: {val:?}")))
}

fn parse_u32(key: &str, val: &str) -> Result<u32, ConfigError> {
    val.parse::<u32>()
        .map_err(|_| ConfigError::invalid(format!("{key} must be an integer: {val:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nodes_env_full() {
        let nodes = parse_nodes_env(
            "local-host|http://host.docker.internal:11434|vram=8,ram=32,gpus=1,cores=16,labels=local:always-on;\
             desk|http://100.1.2.3:11434|vram=8;bare|http://100.9.9.9:11434",
        )
        .unwrap();
        assert_eq!(nodes.len(), 3);
        let local = &nodes[0];
        assert_eq!(local.id.as_str(), "local-host");
        assert_eq!(local.static_capacity.vram_gb, Some(8.0));
        assert_eq!(local.static_capacity.ram_gb, Some(32.0));
        assert_eq!(local.static_capacity.gpus, Some(1));
        assert_eq!(local.static_capacity.cpu_cores, Some(16));
        assert_eq!(local.labels, ["local", "always-on"]);
        assert_eq!(nodes[1].id.as_str(), "desk");
        assert_eq!(nodes[1].static_capacity.vram_gb, Some(8.0));
        assert!(nodes[2].labels.is_empty());
        assert_eq!(nodes[2].static_capacity.vram_gb(), 0.0);
    }

    #[test]
    fn parse_nodes_env_skips_blanks() {
        let nodes = parse_nodes_env("a|http://a:11434;;  ;b|http://b:11434").unwrap();
        assert_eq!(
            nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn parse_nodes_env_capacity_url() {
        let nodes =
            parse_nodes_env("n|http://n:11434|capacity_url=http://n:11436/v1/capacity,vram=8")
                .unwrap();
        assert_eq!(
            nodes[0].capacity_url.as_deref(),
            Some("http://n:11436/v1/capacity")
        );
        assert_eq!(nodes[0].configured_capacity().get("vram_gb"), Some(&8.0));
    }

    #[test]
    fn parse_nodes_env_errors() {
        for bad in [
            "no-pipe",
            "a|http://a|bogus",
            "a|http://a|vram=notanumber",
            "a|http://a|unknown_key=1",
            "a|ftp://a",
            "dup|http://a:11434;dup|http://b:11434",
        ] {
            assert!(parse_nodes_env(bad).is_err(), "{bad}");
        }
    }
}
