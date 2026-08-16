//! Layered YAML + env config loading.

use std::path::{Path, PathBuf};

use serde_yaml::Value;

use crate::fleet::file::{fleet_path_from_env, load_fleet_nodes};
use crate::fleet::state::{FleetState, DEFAULT_STATE_PATH};
use crate::fleet::url_policy::url_host_is_public_ip;

use super::env_source::{EnvSource, OsEnv};
use super::error::ConfigError;
use super::knobs::apply_env_knobs;
use super::merge::deep_merge;
use super::models::{NodeConfig, RouterConfig, YamlTunables};

/// Committed tunables (Verda + optional RunPod; no inventory).
pub const DEFAULTS_YAML: &str = include_str!("router.defaults.yaml");

const ENV_CONFIG: &str = "OLLAMA_ROUTER_CONFIG";
const ENV_STATE: &str = "OLLAMA_ROUTER_STATE_FILE";

/// Load using the process environment.
pub fn load_config(overlay: Option<&Path>) -> Result<RouterConfig, ConfigError> {
    load_config_from(overlay, &OsEnv)
}

/// Load with an injectable environment (tests).
///
/// Path argument wins over `OLLAMA_ROUTER_CONFIG`. A missing overlay file is
/// not an error.
pub fn load_config_from(
    overlay: Option<&Path>,
    env: &impl EnvSource,
) -> Result<RouterConfig, ConfigError> {
    let mut merged = tunables_value_from_defaults()?;

    let overlay_path = overlay.map(Path::to_path_buf).or_else(|| {
        env.var(ENV_CONFIG)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    });

    if let Some(path) = overlay_path {
        if let Some(overlay_raw) = load_yaml_file(&path)? {
            reject_nodes(&overlay_raw, &path.display().to_string())?;
            deep_merge(&mut merged, overlay_raw);
        }
    }

    apply_env_knobs(&mut merged, env)?;
    let tunables = tunables_from_value(merged)?;
    tunables.require_cloud_credentials(env)?;

    let (fleet_path, fleet_missing_is_error) = fleet_path_from_env(env);
    let mut nodes = load_fleet_nodes(&fleet_path, fleet_missing_is_error)?;

    let state_path = env
        .var(ENV_STATE)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_STATE_PATH.to_string());
    let state = FleetState::new(&state_path);
    match state.ensure_created() {
        Ok(()) => {}
        Err(err) if err.is_permission_denied() => {
            tracing::debug!(
                path = %state_path,
                "fleet state file not created (permission denied)"
            );
        }
        Err(err) => return Err(err.into()),
    }
    hydrate_node_urls(&mut nodes, &state)?;

    let mut config = RouterConfig::from_tunables(tunables, nodes);
    config.fleet_path = fleet_path;
    config.fleet_missing_is_error = fleet_missing_is_error;
    config.state_path = PathBuf::from(&state_path);
    config.validate_nodes()?;
    Ok(config)
}

/// Apply FleetState routing URLs onto permanent nodes (public IPs replaced).
///
/// zrok enroll loopback URLs hydrate when fleet.yaml has a public IP (including
/// CGNAT and public IPv6). Loopback, RFC1918, and LAN hostnames stay as written.
pub fn hydrate_node_urls(nodes: &mut [NodeConfig], state: &FleetState) -> Result<(), ConfigError> {
    for node in nodes {
        let persisted = state.hydrate_url(&node.id)?;
        if let Some(persisted) = persisted {
            match node.url.as_deref() {
                None => node.url = Some(persisted),
                Some(existing) if url_host_is_public_ip(existing) => {
                    node.url = Some(persisted);
                }
                Some(_) => {}
            }
        }
        if let Some(cap) = state.hydrate_capacity_url(&node.id)? {
            match node.capacity_url.as_deref() {
                None => node.capacity_url = Some(cap),
                Some(existing) if url_host_is_public_ip(existing) => {
                    node.capacity_url = Some(cap);
                }
                Some(_) => {}
            }
        }
        node.normalize_and_validate()?;
    }
    Ok(())
}

/// Parse YAML text into tunables (empty inventory). Does not read the
/// committed defaults file — missing keys use struct defaults.
pub fn parse_yaml(source: &str) -> Result<RouterConfig, ConfigError> {
    let raw = parse_yaml_value(source, "YAML text")?;
    reject_nodes(&raw, "YAML text")?;
    let mut merged = default_tunables_value()?;
    if !matches!(raw, Value::Null) {
        deep_merge(&mut merged, raw);
    }
    let tunables = tunables_from_value(merged)?;
    Ok(RouterConfig::from_tunables(tunables, Vec::new()))
}

fn tunables_value_from_defaults() -> Result<Value, ConfigError> {
    let mut merged = default_tunables_value()?;
    let defaults = parse_yaml_value(DEFAULTS_YAML, "router.defaults.yaml")?;
    reject_nodes(&defaults, "router.defaults.yaml")?;
    deep_merge(&mut merged, defaults);
    Ok(merged)
}

fn default_tunables_value() -> Result<Value, ConfigError> {
    serde_yaml::to_value(YamlTunables::default())
        .map_err(|e| ConfigError::invalid(format!("serialize defaults: {e}")))
}

fn tunables_from_value(raw: Value) -> Result<YamlTunables, ConfigError> {
    let tunables: YamlTunables = serde_yaml::from_value(raw)
        .map_err(|e| ConfigError::invalid(format!("invalid config: {e}")))?;
    tunables.validate()?;
    Ok(tunables)
}

fn parse_yaml_value(source: &str, origin: &str) -> Result<Value, ConfigError> {
    if source.trim().is_empty() {
        return Ok(Value::Mapping(serde_yaml::Mapping::new()));
    }
    let raw: Value = serde_yaml::from_str(source)
        .map_err(|e| ConfigError::InvalidYaml(format!("{origin}: {e}")))?;
    match &raw {
        Value::Null => Ok(Value::Mapping(serde_yaml::Mapping::new())),
        Value::Mapping(_) => Ok(raw),
        _ => Err(ConfigError::RootNotMapping),
    }
}

fn load_yaml_file(path: &Path) -> Result<Option<Value>, ConfigError> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let origin = path.display().to_string();
    if text.trim().is_empty() {
        return Ok(Some(Value::Mapping(serde_yaml::Mapping::new())));
    }
    let raw: Value = serde_yaml::from_str(&text)
        .map_err(|e| ConfigError::InvalidYaml(format!("invalid YAML in {origin}: {e}")))?;
    match raw {
        Value::Null => Ok(Some(Value::Mapping(serde_yaml::Mapping::new()))),
        Value::Mapping(_) => Ok(Some(raw)),
        _ => Err(ConfigError::RootNotMapping),
    }
}

pub(crate) fn reject_nodes(raw: &Value, source: &str) -> Result<(), ConfigError> {
    let Some(map) = raw.as_mapping() else {
        return Ok(());
    };
    if map.contains_key("nodes") {
        return Err(ConfigError::NodesInventory {
            origin: source.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::models::RequestClass;
    use crate::fleet::state::FleetState;
    use std::collections::HashMap;
    use std::fs;

    const TUNABLES_YAML: &str = r#"
policy:
  medium_min_vram_gb: 8
  sticky_affinity: true
  default_max_inflight: 4
  retry_on_status: [429, 503]
  overload_wait_ms: 250
health:
  interval_seconds: 7.5
  fail_streak_threshold: 2
  request_fail_credit: 2
  overload_fail_credit: 1
timeouts:
  embed_seconds: 120
desired_model_tiers:
  - models: [qwen3-embedding:8b]
    min_vram_gb: 0
ready_requires_embedding_model: true
"#;

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    fn env_with_state(dir: &tempfile::TempDir) -> HashMap<String, String> {
        let mut env = empty_env();
        env.insert(
            ENV_STATE.to_string(),
            dir.path().join("fleet-state.json").display().to_string(),
        );
        let fleet = dir.path().join("empty-fleet.yaml");
        fs::write(&fleet, "version: 1\nnodes: []\n").unwrap();
        env.insert(
            "OLLAMA_ROUTER_FLEET".to_string(),
            fleet.display().to_string(),
        );
        env
    }

    fn write_fleet(dir: &tempfile::TempDir, yaml: &str) -> PathBuf {
        let path = dir.path().join("fleet.yaml");
        fs::write(&path, yaml).unwrap();
        path
    }

    fn env_with_fleet(dir: &tempfile::TempDir, fleet_yaml: &str) -> HashMap<String, String> {
        let mut env = env_with_state(dir);
        let path = write_fleet(dir, fleet_yaml);
        env.insert("OLLAMA_ROUTER_FLEET".into(), path.display().to_string());
        env
    }

    #[test]
    fn parse_yaml_tunables() {
        let config = parse_yaml(TUNABLES_YAML).unwrap();
        assert!(config.nodes.is_empty());
        assert_eq!(config.policy.medium_min_vram_gb, 8.0);
        assert!(config.policy.sticky_affinity);
        assert_eq!(config.policy.default_max_inflight, Some(4));
        assert_eq!(config.policy.retry_max_attempts, 3);
        assert_eq!(config.policy.retry_on_status, vec![429, 503]);
        assert_eq!(config.policy.overload_wait_ms, 250);
        assert_eq!(config.health.interval_seconds, 7.5);
        assert_eq!(config.health.fail_streak_threshold, 2);
        assert_eq!(config.health.request_fail_credit, 2);
        assert_eq!(config.health.overload_fail_credit, 1);
        assert_eq!(config.timeouts.embed_seconds, 120.0);
        assert_eq!(config.desired_model_tiers[0].models, ["qwen3-embedding:8b"]);
        assert!(config.ready_requires_embedding_model);
    }

    #[test]
    fn parse_yaml_rejects_nodes_inventory() {
        let err = parse_yaml(
            "nodes:\n  - id: n\n    url: http://n:11434\npolicy:\n  sticky_affinity: true\n",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::NodesInventory { .. }));
        assert!(err.to_string().contains("OLLAMA_ROUTER_FLEET"));
    }

    #[test]
    fn parse_yaml_rejects_empty_and_null_nodes() {
        assert!(matches!(
            parse_yaml("nodes: []\n").unwrap_err(),
            ConfigError::NodesInventory { .. }
        ));
        assert!(matches!(
            parse_yaml("nodes: null\n").unwrap_err(),
            ConfigError::NodesInventory { .. }
        ));
    }

    #[test]
    fn parse_yaml_defaults() {
        let config = parse_yaml("").unwrap();
        assert!(config.nodes.is_empty());
        assert_eq!(config.policy.small_max_b, 4.0);
        assert_eq!(config.health.interval_seconds, 5.0);
        assert_eq!(config.max_pulls_per_node, 1);
        assert_eq!(config.listen_port, 11434);
        assert_eq!(config.policy.default_max_inflight, None);
        assert_eq!(config.policy.retry_max_attempts, 3);
        assert_eq!(config.policy.retry_on_status, vec![429, 503]);
        assert_eq!(config.policy.overload_wait_ms, 0);
        assert_eq!(config.health.overload_fail_credit, 1);
        assert_eq!(config.policy.saturated_retry_after_seconds, 30);
        assert_eq!(config.policy.provision_retry_after_seconds, 30);
        assert!(!config.policy.auto_pull_on_miss);
        assert_eq!(config.policy.pull_miss_retry_after_seconds, 10);
        assert_eq!(config.policy.auto_pull_wait_seconds, 0.0);
        assert!((config.health.probe_jitter_ratio - 0.2).abs() < f64::EPSILON);
        assert_eq!(config.health.max_concurrent_probes, 8);
        assert_eq!(config.health.max_probe_body_bytes, 8 * 1024 * 1024);
        assert!(config.desired_model_tiers.is_empty());
        assert_eq!(config.verda.min_vram_gb, 8.0);
        assert_eq!(config.verda.max_vram_gb, Some(80.0));
        assert_eq!(config.verda.ssh_key_name, "ollama-router");
        assert!(config.policy.model_warm_enabled);
        assert_eq!(config.policy.model_warm_interval_seconds, 60.0);
        assert_eq!(config.upstream.max_connections, 256);
        assert_eq!(config.upstream.max_keepalive_connections, 32);
        assert_eq!(
            config.policy.reject_on_ram_elevated_for_classes,
            [RequestClass::Medium, RequestClass::Large]
        );
    }

    #[test]
    fn desired_model_tiers_parsed() {
        let config = parse_yaml(
            "desired_model_tiers:\n  - models: [embed:8b, tiny:1b]\n    min_vram_gb: 0\n  - models: [mid:7b]\n    min_vram_gb: 24\n",
        )
        .unwrap();
        let tiers = config.effective_model_tiers();
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].models, ["embed:8b", "tiny:1b"]);
        assert_eq!(tiers[1].min_vram_gb, 24.0);
        assert_eq!(
            config.tier_models_for_vram(0.0),
            vec!["embed:8b".to_string(), "tiny:1b".to_string()]
        );
        assert_eq!(config.tier_models_for_vram(24.0).len(), 3);
    }

    #[test]
    fn parse_yaml_rejects_desired_models_unknown_field() {
        let err = parse_yaml("desired_models:\n  - qwen3-embedding:8b\n").unwrap_err();
        assert!(err.to_string().contains("desired_models") || err.to_string().contains("unknown"));
    }

    #[test]
    fn ram_policy_invalid_thresholds_rejected() {
        assert!(parse_yaml("policy:\n  ram_headroom: 1.5\n").is_err());
        assert!(parse_yaml("policy:\n  reject_on_ram_elevated_for_classes: [bogus]\n").is_err());
    }

    #[test]
    fn auto_pull_policy_bounds_rejected() {
        assert!(parse_yaml("policy:\n  auto_pull_wait_seconds: 121\n").is_err());
        assert!(parse_yaml("policy:\n  pull_miss_retry_after_seconds: 0\n").is_err());
        assert!(parse_yaml("policy:\n  pull_miss_retry_after_seconds: 901\n").is_err());
    }

    #[test]
    fn env_auto_pull_on_miss_knob() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env_with_state(&dir);
        env.insert("OLLAMA_ROUTER_AUTO_PULL_ON_MISS".into(), "true".into());
        env.insert(
            "OLLAMA_ROUTER_PULL_MISS_RETRY_AFTER_SECONDS".into(),
            "7".into(),
        );
        env.insert("OLLAMA_ROUTER_AUTO_PULL_WAIT_SECONDS".into(), "1.5".into());
        let config = load_config_from(None, &env).unwrap();
        assert!(config.policy.auto_pull_on_miss);
        assert_eq!(config.policy.pull_miss_retry_after_seconds, 7);
        assert!((config.policy.auto_pull_wait_seconds - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn env_zrok_tunnel_knobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env_with_state(&dir);
        env.insert(
            "OLLAMA_ROUTER_ZROK_API_ENDPOINT".into(),
            "http://127.0.0.1:18080".into(),
        );
        env.insert(
            "OLLAMA_ROUTER_ZROK_ENABLE_TOKEN_ENV".into(),
            "MY_ZROK_TOKEN".into(),
        );
        let config = load_config_from(None, &env).unwrap();
        assert_eq!(config.tunnel.api_endpoint(), Some("http://127.0.0.1:18080"));
        assert_eq!(config.tunnel.enable_token_env, "MY_ZROK_TOKEN");
        assert_eq!(config.tunnel.access_bind, "127.0.0.1");
    }

    #[test]
    fn verda_vram_bounds_rejected() {
        assert!(parse_yaml("verda:\n  max_vram_gb: -1\n").is_err());
        assert!(parse_yaml("verda:\n  min_vram_gb: 48\n  max_vram_gb: 24\n").is_err());
    }

    #[test]
    fn non_finite_policy_and_capacity_rejected() {
        assert!(parse_yaml("policy:\n  inflight_weight: .nan\n").is_err());
        assert!(parse_yaml("policy:\n  ram_headroom: .inf\n").is_err());
    }

    #[test]
    fn thunder_overlay_is_unknown_field() {
        assert!(parse_yaml("thunder:\n  enabled: true\n").is_err());
    }

    #[test]
    fn runpod_overlay_parses_when_disabled() {
        let config = parse_yaml("runpod:\n  enabled: false\n").unwrap();
        assert!(!config.runpod.enabled);
        assert_eq!(config.runpod.api_key_env, "RUNPOD_API_KEY");
    }

    #[test]
    fn runpod_enabled_without_api_key_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("overlay.yaml");
        fs::write(&overlay, "runpod:\n  enabled: true\n").unwrap();
        let err = load_config_from(Some(&overlay), &env_with_state(&dir)).unwrap_err();
        assert!(err.to_string().contains("RUNPOD_API_KEY"), "err={err}");
    }

    #[test]
    fn runpod_enabled_with_api_key_loads() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("overlay.yaml");
        fs::write(&overlay, "runpod:\n  enabled: true\n").unwrap();
        let mut env = env_with_state(&dir);
        env.insert("RUNPOD_API_KEY".into(), "test-key".into());
        let config = load_config_from(Some(&overlay), &env).unwrap();
        assert!(config.runpod.enabled);
    }

    #[test]
    fn max_price_caps_must_be_positive() {
        assert!(parse_yaml("verda:\n  max_spot_price_per_hour: 0\n").is_err());
        assert!(parse_yaml("verda:\n  max_spot_price_per_hour: -1\n").is_err());
        assert!(parse_yaml("runpod:\n  max_price_per_hour: 0\n").is_err());
        assert!(parse_yaml("runpod:\n  max_price_per_hour: -0.5\n").is_err());
        let ok = parse_yaml("verda:\n  max_spot_price_per_hour: 0.5\n").unwrap();
        assert_eq!(ok.verda.max_spot_price_per_hour, Some(0.5));
    }

    #[test]
    fn invalid_yaml_and_non_mapping_root() {
        assert!(matches!(
            parse_yaml("policy: [unclosed").unwrap_err(),
            ConfigError::InvalidYaml(_)
        ));
        assert!(matches!(
            parse_yaml("- just\n- a list\n").unwrap_err(),
            ConfigError::RootNotMapping
        ));
    }

    #[test]
    fn env_name_literal_rejected() {
        let err = parse_yaml("verda:\n  client_id_env: not-a-valid-env\n").unwrap_err();
        assert!(err.to_string().contains("client_id_env"));
    }

    #[test]
    fn committed_defaults_have_no_inventory_or_other_clouds() {
        let raw: Value = serde_yaml::from_str(DEFAULTS_YAML).unwrap();
        let map = raw.as_mapping().unwrap();
        assert!(!map.contains_key("nodes"));
        assert!(!map.contains_key("thunder"));
        assert!(map.contains_key("runpod"));
        let tunables: YamlTunables = serde_yaml::from_value(raw).unwrap();
        tunables.validate().unwrap();
        assert!(!tunables.runpod.enabled);
        assert_eq!(tunables.runpod.min_lifetime_seconds, 0.0);
        assert_eq!(tunables.verda.min_lifetime_seconds, 0.0);
        assert_eq!(
            tunables.verda.selection_strategy,
            crate::config::SelectionStrategy::BestValue
        );
        assert_eq!(tunables.verda.min_vram_gb, 8.0);
        assert_eq!(tunables.verda.max_vram_gb, Some(80.0));
        assert_eq!(tunables.verda.ssh_key_name, "ollama-router");
        assert_eq!(tunables.tunnel.zrok_bin, "zrok");
        assert!(tunables
            .tunnel
            .public_share_suffixes
            .iter()
            .any(|s| s.contains("zrok.io")));
        assert!(tunables
            .tunnel
            .public_share_suffixes
            .iter()
            .any(|s| s.contains("proxy.runpod.net")));
        assert!(tunables.tunnel.api_endpoint.trim().is_empty());
        assert_eq!(tunables.tunnel.enable_token_env, "ZROK_ENABLE_TOKEN");
        assert_eq!(tunables.tunnel.access_bind, "127.0.0.1");
    }

    #[test]
    fn overlay_deep_merge_keeps_nested_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("overlay.yaml");
        fs::write(&overlay, "verda:\n  enabled: true\n").unwrap();
        let env = env_with_state(&dir);
        let config = load_config_from(Some(&overlay), &env).unwrap();
        assert!(config.verda.enabled);
        assert_eq!(config.verda.min_vram_gb, 8.0);
    }

    #[test]
    fn load_config_reads_fleet_file() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("overlay.yaml");
        fs::write(&overlay, TUNABLES_YAML).unwrap();
        let env = env_with_fleet(
            &dir,
            "version: 1\nnodes:\n  - id: desk\n    url: http://env:11434\n    capacity:\n      vram_gb: 4\n",
        );
        let config = load_config_from(Some(&overlay), &env).unwrap();
        assert_eq!(config.nodes.len(), 1);
        assert_eq!(config.nodes[0].id.as_str(), "desk");
        assert_eq!(config.nodes[0].static_capacity.vram_gb, Some(4.0));
        assert_eq!(config.health.interval_seconds, 7.5);
        assert!(config.fleet_missing_is_error);
    }

    #[test]
    fn load_config_rejects_nodes_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = dir.path().join("legacy.yaml");
        fs::write(
            &overlay,
            "nodes:\n  - id: legacy\n    url: http://legacy:11434\npolicy:\n  sticky_affinity: true\n",
        )
        .unwrap();
        let err = load_config_from(Some(&overlay), &env_with_state(&dir)).unwrap_err();
        assert!(matches!(err, ConfigError::NodesInventory { .. }));
    }

    #[test]
    fn load_config_missing_overlay_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.yaml");
        let config = load_config_from(Some(&missing), &env_with_state(&dir)).unwrap();
        assert!(config.nodes.is_empty());
        assert_eq!(config.policy.default_max_inflight, None);
        assert!(!config.verda.enabled);
        assert!(config.fleet_missing_is_error);
        let created = dir.path().join("fleet-state.json");
        assert!(
            created.is_file(),
            "first load should create an empty fleet-state.json"
        );
    }

    #[test]
    fn missing_explicit_fleet_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env_with_state(&dir);
        env.insert(
            "OLLAMA_ROUTER_FLEET".into(),
            dir.path().join("missing-fleet.yaml").display().to_string(),
        );
        let err = load_config_from(None, &env).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn hydrate_public_ipv4_replaced_overlay_and_hostname_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env_with_fleet(
            &dir,
            "version: 1\nnodes:\n  - id: host-01\n    url: http://8.8.8.8:11434\n",
        );
        let state = FleetState::new(env.get(ENV_STATE).unwrap());
        state
            .persist_url("host-01", "http://127.0.0.1:41990")
            .unwrap();

        let config = load_config_from(None, &env).unwrap();
        assert_eq!(
            config.nodes[0].url.as_deref(),
            Some("http://127.0.0.1:41990")
        );

        let fleet = write_fleet(
            &dir,
            "version: 1\nnodes:\n  - id: host-01\n    url: http://10.0.0.9:11434\n",
        );
        env.insert("OLLAMA_ROUTER_FLEET".into(), fleet.display().to_string());
        let config = load_config_from(None, &env).unwrap();
        assert_eq!(
            config.nodes[0].url.as_deref(),
            Some("http://10.0.0.9:11434")
        );

        let fleet = write_fleet(
            &dir,
            "version: 1\nnodes:\n  - id: host-01\n    url: http://host.docker.internal:11434\n",
        );
        env.insert("OLLAMA_ROUTER_FLEET".into(), fleet.display().to_string());
        let config = load_config_from(None, &env).unwrap();
        assert_eq!(
            config.nodes[0].url.as_deref(),
            Some("http://host.docker.internal:11434")
        );
    }

    #[test]
    fn hydrate_zrok_loopback_onto_public_ipv4_node() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_with_fleet(
            &dir,
            "version: 1\nnodes:\n  - id: host-01\n    url: http://8.8.8.8:11434\n",
        );
        let state = FleetState::new(env.get(ENV_STATE).unwrap());
        state
            .persist_enroll(
                "host-01",
                crate::fleet::state::EnrollPersist {
                    url: "http://127.0.0.1:41990",
                    capacity_url: "http://127.0.0.1:41991",
                    ollama_share_id: "share-ollama",
                    agent_share_id: "share-agent",
                },
            )
            .unwrap();
        let config = load_config_from(None, &env).unwrap();
        assert_eq!(
            config.nodes[0].url.as_deref(),
            Some("http://127.0.0.1:41990")
        );
        assert_eq!(
            config.nodes[0].capacity_url.as_deref(),
            Some("http://127.0.0.1:41991")
        );
    }

    #[test]
    fn malformed_knobs_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        for (key, value) in [
            ("VERDA_ENABLED", "perhaps"),
            ("OLLAMA_ROUTER_DEBUG_HEADERS", "sometimes"),
            ("VERDA_MIN_VRAM_GB", "nan"),
        ] {
            let mut env = env_with_state(&dir);
            env.insert(key.into(), value.into());
            assert!(load_config_from(None, &env).is_err(), "{key}={value}");
        }
    }

    #[test]
    fn verda_demand_scale_price_from_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env_with_state(&dir);
        env.insert("VERDA_DEMAND_SCALE_PRICE_PER_HOUR".into(), "0.30".into());
        let config = load_config_from(None, &env).unwrap();
        assert_eq!(config.verda.demand_scale_price_per_hour, Some(0.30));
    }

    #[test]
    fn overlay_example_file_has_no_inventory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.overlay.example.yaml");
        let text = fs::read_to_string(&root).unwrap();
        assert!(!text.lines().any(|l| l.starts_with("nodes:")));
        assert!(!text.lines().any(|l| l.starts_with("thunder:")));
        // Commented `# runpod:` example lines are allowed; active top-level key is not.
        assert!(!text.lines().any(|l| l.starts_with("runpod:")));
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn yaml_mapping_with_nodes_always_errors(
            extra in prop::collection::hash_map("[a-z]{1,6}", 0i32..8, 0..3),
            kind in 0u8..5
        ) {
            let nodes = match kind {
                0 => "[]",
                1 => "null",
                2 => "[{id: n, url: http://n:11434}]",
                3 => "foo",
                _ => "0",
            };
            let mut yaml = format!("nodes: {nodes}\n");
            for (k, v) in extra {
                if k == "nodes" {
                    continue;
                }
                yaml.push_str(&format!("{k}: {v}\n"));
            }
            let err = parse_yaml(&yaml).unwrap_err();
            let ok = matches!(err, ConfigError::NodesInventory { .. });
            prop_assert!(ok);
        }
    }
}
