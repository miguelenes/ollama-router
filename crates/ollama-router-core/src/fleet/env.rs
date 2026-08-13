//! Parse `OLLAMA_HOST_NN_*` env fleet into [`NodeConfig`] list.

use std::collections::BTreeMap;
use std::path::Path;

use url::Url;

use crate::config::env_source::EnvSource;
use crate::config::error::ConfigError;
use crate::config::models::{Capacity, NodeConfig, NodeProvisionConfig, NodeSshConfig};
use crate::fleet::ids::NodeId;

const PREFIX: &str = "OLLAMA_HOST_";
const DEFAULT_SSH_KEY: &str = "/home/router/.ssh/id_ed25519";

fn handler_for(field: &str) -> Option<&'static str> {
    match field {
        "ID" => Some("id"),
        "URL" => Some("url"),
        "LABELS" => Some("labels"),
        "CAPACITY_URL" => Some("capacity_url"),
        "CAPACITY_PORT" => Some("capacity_port"),
        "SSH_HOST" => Some("ssh_host"),
        "SSH_PORT" => Some("ssh_port"),
        "SSH_USER" => Some("ssh_user"),
        "SSH_KEY_FILE" => Some("ssh_key_file"),
        "SSH_PASSWORD_ENV" => Some("ssh_password_env"),
        "PROVISION" => Some("provision"),
        "PROVISION_OS_UPGRADE" => Some("provision_os_upgrade"),
        "PROVISION_SKIP_MODELS" => Some("provision_skip_models"),
        "TS_HOSTNAME" => Some("ts_hostname"),
        "TS_TAGS" => Some("ts_tags"),
        "TS_ADVERTISE_ROUTES" => Some("ts_advertise_routes"),
        "VRAM_GB" => Some("vram_gb"),
        "RAM_GB" => Some("ram_gb"),
        "GPUS" => Some("gpus"),
        "CPU_CORES" => Some("cpu_cores"),
        "MAX_INFLIGHT" => Some("max_inflight"),
        _ => None,
    }
}

const SSH_KEYS: &[&str] = &[
    "ssh_host",
    "ssh_port",
    "ssh_user",
    "ssh_key_file",
    "ssh_password_env",
];

const CAPACITY_KEYS: &[&str] = &["vram_gb", "ram_gb", "gpus", "cpu_cores"];

const PROVISION_KEYS: &[&str] = &[
    "provision",
    "provision_os_upgrade",
    "provision_skip_models",
    "ts_hostname",
    "ts_tags",
    "ts_advertise_routes",
];

/// Parse `OLLAMA_HOST_NN_*` (NN in 1..=99). Empty fleet is valid.
pub fn parse_host_environ(env: &impl EnvSource) -> Result<Vec<NodeConfig>, ConfigError> {
    let mut hosts: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for (env_key, raw_value) in env.vars() {
        let Some((index, field)) = match_host_key(&env_key)? else {
            continue;
        };
        let Some(handler) = handler_for(&field) else {
            continue;
        };
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        hosts
            .entry(index)
            .or_default()
            .insert(handler.to_string(), value.to_string());
    }

    let mut nodes = Vec::new();
    for (index, raw) in hosts {
        if !is_configured(&raw) {
            continue;
        }
        nodes.push(build_node(index, &raw)?);
    }
    crate::config::models::reject_duplicate_node_ids(&nodes)?;
    Ok(nodes)
}

fn match_host_key(env_key: &str) -> Result<Option<(u32, String)>, ConfigError> {
    let upper = env_key.to_ascii_uppercase();
    let rest = match upper.strip_prefix(PREFIX) {
        Some(rest) => rest,
        None => return Ok(None),
    };
    let Some((digits, field)) = rest.split_once('_') else {
        return Ok(None);
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) || field.is_empty() {
        return Ok(None);
    }
    let index: u32 = digits.parse().map_err(|_| ConfigError::HostIndex {
        key: env_key.to_string(),
        index: u32::MAX,
    })?;
    if !(1..=99).contains(&index) {
        return Err(ConfigError::HostIndex {
            key: env_key.to_string(),
            index,
        });
    }
    Ok(Some((index, field.to_string())))
}

fn is_configured(raw: &BTreeMap<String, String>) -> bool {
    raw.contains_key("url") || raw.contains_key("ssh_host")
}

fn boolish(raw: &str) -> bool {
    matches!(raw.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

fn int_positive(raw: &str) -> Option<u32> {
    raw.parse::<u32>().ok().filter(|v| *v > 0)
}

fn float_non_negative(raw: &str) -> Option<f64> {
    raw.parse::<f64>().ok().filter(|v| *v >= 0.0)
}

fn derive_capacity_url(
    ollama_url: Option<&str>,
    ssh_host: Option<&str>,
    port: u32,
) -> Option<String> {
    let mut scheme = String::from("http");
    let mut host = None;
    if let Some(url) = ollama_url {
        if let Ok(parsed) = Url::parse(url) {
            host = parsed.host_str().map(str::to_string);
            if parsed.scheme() == "http" || parsed.scheme() == "https" {
                scheme = parsed.scheme().to_string();
            }
        }
    }
    if host.is_none() {
        host = ssh_host
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    let host = host?;
    Some(format!("{scheme}://{host}:{port}/v1/capacity"))
}

fn build_node(index: u32, raw: &BTreeMap<String, String>) -> Result<NodeConfig, ConfigError> {
    let default_id = format!("host-{index:02}");
    let node_id = NodeId::parse(raw.get("id").map(String::as_str).unwrap_or(&default_id))
        .map_err(ConfigError::invalid)?;

    let url = raw.get("url").cloned();

    let mut capacity_url = raw.get("capacity_url").cloned();
    if capacity_url.is_none() {
        if let Some(port_raw) = raw.get("capacity_port") {
            if let Some(port) = int_positive(port_raw) {
                capacity_url = derive_capacity_url(
                    url.as_deref(),
                    raw.get("ssh_host").map(String::as_str),
                    port,
                );
            }
        }
    }

    let labels = raw
        .get("labels")
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut static_capacity = Capacity::default();
    for key in CAPACITY_KEYS {
        if let Some(raw_val) = raw.get(*key) {
            match *key {
                "vram_gb" => static_capacity.vram_gb = float_non_negative(raw_val),
                "ram_gb" => static_capacity.ram_gb = float_non_negative(raw_val),
                "gpus" => {
                    static_capacity.gpus = float_non_negative(raw_val).and_then(|v| {
                        if v.fract() == 0.0 && v <= f64::from(u32::MAX) {
                            Some(v as u32)
                        } else {
                            None
                        }
                    });
                }
                "cpu_cores" => {
                    static_capacity.cpu_cores = float_non_negative(raw_val).and_then(|v| {
                        if v.fract() == 0.0 && v <= f64::from(u32::MAX) {
                            Some(v as u32)
                        } else {
                            None
                        }
                    });
                }
                _ => {}
            }
        }
    }

    let max_inflight = raw.get("max_inflight").and_then(|v| int_positive(v));

    let provision = if PROVISION_KEYS.iter().any(|k| raw.contains_key(*k)) {
        Some(NodeProvisionConfig {
            enabled: boolish(raw.get("provision").map(String::as_str).unwrap_or("1")),
            os_upgrade: boolish(
                raw.get("provision_os_upgrade")
                    .map(String::as_str)
                    .unwrap_or("1"),
            ),
            skip_models: boolish(
                raw.get("provision_skip_models")
                    .map(String::as_str)
                    .unwrap_or("0"),
            ),
            skip_ollama: false,
            ts_ephemeral: false,
            ts_accept_routes: false,
            ts_hostname: raw.get("ts_hostname").cloned(),
            ts_tags: raw.get("ts_tags").cloned(),
            ts_advertise_routes: raw.get("ts_advertise_routes").cloned(),
        })
    } else {
        None
    };

    let ssh = if SSH_KEYS.iter().any(|k| raw.contains_key(*k)) {
        let ssh_host = raw.get("ssh_host").cloned().unwrap_or_default();
        if ssh_host.is_empty() {
            None
        } else {
            let mut key_file = raw.get("ssh_key_file").cloned();
            if key_file.is_none()
                && provision.as_ref().is_some_and(|p| p.enabled)
                && Path::new(DEFAULT_SSH_KEY).is_file()
            {
                key_file = Some(DEFAULT_SSH_KEY.to_string());
            }
            let port_u32 =
                int_positive(raw.get("ssh_port").map(String::as_str).unwrap_or("22")).unwrap_or(22);
            let port = u16::try_from(port_u32)
                .map_err(|_| ConfigError::invalid("ssh.port must be between 1 and 65535"))?;
            Some(NodeSshConfig {
                host: ssh_host,
                port,
                user: raw
                    .get("ssh_user")
                    .cloned()
                    .unwrap_or_else(|| "root".to_string()),
                key_file,
                password_env: raw.get("ssh_password_env").cloned(),
            })
        }
    } else {
        None
    };

    let mut node = NodeConfig {
        id: node_id,
        url,
        capacity_url,
        labels,
        static_capacity,
        max_inflight,
        ssh,
        provision,
    };
    node.normalize_and_validate()?;
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn two_hosts_url_only() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_ID", "local"),
            ("OLLAMA_HOST_01_URL", "http://host.docker.internal:11434"),
            ("OLLAMA_HOST_01_LABELS", "cpu,always-on"),
            ("OLLAMA_HOST_02_ID", "nuc"),
            ("OLLAMA_HOST_02_URL", "http://100.106.14.5:11434"),
            ("OLLAMA_HOST_02_LABELS", "gpu"),
        ]))
        .unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id.as_str(), "local");
        assert_eq!(
            nodes[0].url.as_deref(),
            Some("http://host.docker.internal:11434")
        );
        assert_eq!(nodes[0].labels, ["cpu", "always-on"]);
        assert_eq!(nodes[0].max_inflight, None);
        assert_eq!(nodes[0].static_capacity.vram_gb(), 0.0);
        assert_eq!(nodes[1].id.as_str(), "nuc");
    }

    #[test]
    fn default_id_when_omitted() {
        let nodes = parse_host_environ(&env(&[("OLLAMA_HOST_01_URL", "http://example.com:11434")]))
            .unwrap();
        assert_eq!(nodes[0].id.as_str(), "host-01");
    }

    #[test]
    fn ssh_only_and_defaults() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_SSH_HOST", "203.0.113.10"),
            ("OLLAMA_HOST_01_SSH_USER", "root"),
            ("OLLAMA_HOST_01_SSH_PORT", "2222"),
            ("OLLAMA_HOST_01_SSH_KEY_FILE", "/home/you/.ssh/id_ed25519"),
            ("OLLAMA_HOST_01_PROVISION", "1"),
        ]))
        .unwrap();
        let node = &nodes[0];
        assert!(node.url.is_none());
        let ssh = node.ssh.as_ref().unwrap();
        assert_eq!(ssh.host, "203.0.113.10");
        assert_eq!(ssh.port, 2222);
        assert!(node.provision.as_ref().unwrap().enabled);

        let defaults =
            parse_host_environ(&env(&[("OLLAMA_HOST_01_SSH_HOST", "10.0.0.1")])).unwrap();
        let ssh = defaults[0].ssh.as_ref().unwrap();
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.user, "root");
    }

    #[test]
    fn provision_and_ts_fields() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_SSH_HOST", "10.0.0.1"),
            ("OLLAMA_HOST_01_PROVISION", "1"),
            ("OLLAMA_HOST_01_PROVISION_OS_UPGRADE", "true"),
            ("OLLAMA_HOST_01_PROVISION_SKIP_MODELS", "1"),
            ("OLLAMA_HOST_01_TS_HOSTNAME", "my-node"),
            ("OLLAMA_HOST_01_TS_TAGS", "tag:gpu"),
            (
                "OLLAMA_HOST_01_TS_ADVERTISE_ROUTES",
                "10.0.0.0/24,192.168.1.0/24",
            ),
        ]))
        .unwrap();
        let p = nodes[0].provision.as_ref().unwrap();
        assert!(p.enabled);
        assert!(p.os_upgrade);
        assert!(p.skip_models);
        assert_eq!(p.ts_hostname.as_deref(), Some("my-node"));
        assert_eq!(
            p.ts_advertise_routes.as_deref(),
            Some("10.0.0.0/24,192.168.1.0/24")
        );
    }

    #[test]
    fn padding_mix_and_capacity() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_ID", "first"),
            ("OLLAMA_HOST_01_URL", "http://f:11434"),
            ("OLLAMA_HOST_2_ID", "second"),
            ("OLLAMA_HOST_2_URL", "http://s:11434"),
            ("OLLAMA_HOST_01_VRAM_GB", "24"),
            ("OLLAMA_HOST_01_RAM_GB", "64"),
            ("OLLAMA_HOST_01_GPUS", "2"),
            ("OLLAMA_HOST_01_CPU_CORES", "16"),
            ("OLLAMA_HOST_01_MAX_INFLIGHT", "8"),
        ]))
        .unwrap();
        assert_eq!(nodes[0].id.as_str(), "first");
        assert_eq!(nodes[1].id.as_str(), "second");
        assert_eq!(nodes[0].static_capacity.vram_gb, Some(24.0));
        assert_eq!(nodes[0].max_inflight, Some(8));
    }

    #[test]
    fn index_00_and_100_are_errors() {
        let err =
            parse_host_environ(&env(&[("OLLAMA_HOST_00_URL", "http://x:11434")])).unwrap_err();
        assert!(matches!(err, ConfigError::HostIndex { index: 0, .. }));
        let err =
            parse_host_environ(&env(&[("OLLAMA_HOST_100_URL", "http://x:11434")])).unwrap_err();
        assert!(matches!(err, ConfigError::HostIndex { index: 100, .. }));
    }

    #[test]
    fn capacity_port_derives_and_explicit_wins() {
        let from_url = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_URL", "http://100.64.0.9:11434"),
            ("OLLAMA_HOST_01_CAPACITY_PORT", "11436"),
        ]))
        .unwrap();
        assert_eq!(
            from_url[0].capacity_url.as_deref(),
            Some("http://100.64.0.9:11436/v1/capacity")
        );

        let from_ssh = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_SSH_HOST", "203.0.113.10"),
            ("OLLAMA_HOST_01_CAPACITY_PORT", "11436"),
            ("OLLAMA_HOST_01_PROVISION", "1"),
        ]))
        .unwrap();
        assert_eq!(
            from_ssh[0].capacity_url.as_deref(),
            Some("http://203.0.113.10:11436/v1/capacity")
        );

        let explicit = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_URL", "http://x:11434"),
            (
                "OLLAMA_HOST_01_CAPACITY_URL",
                "http://custom:9999/v1/capacity",
            ),
            ("OLLAMA_HOST_01_CAPACITY_PORT", "11436"),
        ]))
        .unwrap();
        assert_eq!(
            explicit[0].capacity_url.as_deref(),
            Some("http://custom:9999/v1/capacity")
        );
    }

    #[test]
    fn empty_slots_and_unknown_keys() {
        assert!(parse_host_environ(&env(&[])).unwrap().is_empty());
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_MAX_INFLIGHT", "2"),
            ("OLLAMA_HOST_01_SSH_PORT", "22"),
            ("OLLAMA_HOST_01_SSH_USER", "root"),
            (
                "OLLAMA_HOST_01_SSH_KEY_FILE",
                "/home/router/.ssh/id_ed25519",
            ),
            ("OLLAMA_HOST_03_ID", "gpu-box"),
            ("OLLAMA_HOST_03_URL", "http://100.64.0.9:11434"),
            ("OLLAMA_HOST_03_MAX_INFLIGHT", "2"),
            ("OLLAMA_HOST_03_FICTIONAL_FIELD", "bogus"),
        ]))
        .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.as_str(), "gpu-box");
        assert_eq!(nodes[0].max_inflight, Some(2));
    }

    #[test]
    fn duplicate_ids_are_errors() {
        let err = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_ID", "same"),
            ("OLLAMA_HOST_01_URL", "http://a:11434"),
            ("OLLAMA_HOST_02_ID", "same"),
            ("OLLAMA_HOST_02_URL", "http://b:11434"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("unique"));
    }

    #[test]
    fn max_inflight_invalid_ignored() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_URL", "http://x:11434"),
            ("OLLAMA_HOST_01_MAX_INFLIGHT", "not-a-number"),
        ]))
        .unwrap();
        assert_eq!(nodes[0].max_inflight, None);
    }

    #[test]
    fn goal_example_three_hosts() {
        let nodes = parse_host_environ(&env(&[
            ("OLLAMA_HOST_01_ID", "local"),
            ("OLLAMA_HOST_01_URL", "http://host.docker.internal:11434"),
            ("OLLAMA_HOST_01_LABELS", "cpu,always-on"),
            ("OLLAMA_HOST_02_ID", "nuc"),
            ("OLLAMA_HOST_02_URL", "http://100.106.14.5:11434"),
            ("OLLAMA_HOST_02_LABELS", "gpu"),
            ("OLLAMA_HOST_03_ID", "loud-seed"),
            ("OLLAMA_HOST_03_SSH_HOST", "135.181.63.161"),
            ("OLLAMA_HOST_03_SSH_USER", "root"),
            ("OLLAMA_HOST_03_SSH_PORT", "22"),
            ("OLLAMA_HOST_03_PROVISION", "1"),
            ("OLLAMA_HOST_03_LABELS", "gpu,remote"),
        ]))
        .unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes[0].ssh.is_none());
        assert!(nodes[2].url.is_none());
        assert_eq!(nodes[2].ssh.as_ref().unwrap().host, "135.181.63.161");
        assert!(nodes[2].provision.as_ref().unwrap().enabled);
    }
}
