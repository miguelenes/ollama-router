//! Placement-eligible node sets for pull/delete targeting.

use std::collections::{BTreeMap, HashSet};

use crate::config::PolicyConfig;
use crate::fleet::ids::NodeId;
use crate::fleet::registry::{NodeSnapshot, PressureLevel};
use crate::routing::classify::{classify, RequestClass};
use crate::routing::rank::{capacity_preference, label_ok, static_capacity_fits};

/// Bypass placement and target every configured node.
pub const TARGET_ALL: &str = "#all";

/// Why [`resolve_target_nodes`] rejected the selector.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlacementError {
    #[error("unknown node id: {0}")]
    UnknownNode(String),
}

/// How callers name the target set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetSpec {
    /// `*` / omitted — placement-eligible per model.
    Placement,
    /// `#all` — every configured node (capacity skips still apply at run).
    All,
    /// Explicit ids (validated).
    Nodes(Vec<NodeId>),
}

impl TargetSpec {
    /// Parse admin/CLI node selectors.
    pub fn parse(raw: Option<&[String]>) -> Result<Self, PlacementError> {
        let Some(items) = raw else {
            return Ok(Self::Placement);
        };
        if items.is_empty() || items.iter().all(|s| s.trim().is_empty()) {
            return Ok(Self::Placement);
        }
        if items.len() == 1 && items[0].trim() == "*" {
            return Ok(Self::Placement);
        }
        if items.len() == 1 && items[0].trim() == TARGET_ALL {
            return Ok(Self::All);
        }
        if items.iter().any(|s| s.trim() == "*") {
            return Ok(Self::Placement);
        }
        let mut resolved = Vec::new();
        let mut seen = HashSet::new();
        for item in items {
            let id = item.trim();
            if id.is_empty() || id == TARGET_ALL {
                continue;
            }
            if seen.insert(id.to_string()) {
                resolved.push(
                    NodeId::parse(id).map_err(|_| PlacementError::UnknownNode(id.to_string()))?,
                );
            }
        }
        Ok(Self::Nodes(resolved))
    }

    /// Single string (`*`, `#all`, or unused).
    pub fn parse_one(raw: Option<&str>) -> Result<Self, PlacementError> {
        match raw.map(str::trim) {
            None | Some("") | Some("*") => Ok(Self::Placement),
            Some(TARGET_ALL) => Ok(Self::All),
            Some(other) => Self::parse(Some(&[other.to_string()])),
        }
    }
}

/// Request class a pull/placement decision should honour for `model`.
pub fn placement_class(model: &str, policy: &PolicyConfig) -> RequestClass {
    classify("/api/generate", Some(model), policy)
}

/// Whether transient RAM pressure should block a model pull.
///
/// Trusts the agent's `pressure_level`. Critical blocks when
/// `reject_on_ram_critical`; elevated blocks only when `strict`.
pub fn ram_pressure_blocks_placement(
    node: &NodeSnapshot,
    request_class: RequestClass,
    policy: &PolicyConfig,
    strict: bool,
) -> bool {
    let Some(policy_class) = request_class.as_policy_class() else {
        return false;
    };
    if !policy.ram_sensitive_classes.contains(&policy_class) {
        return false;
    }
    match node.pressure_level {
        PressureLevel::Critical => policy.reject_on_ram_critical,
        PressureLevel::Elevated => strict,
        PressureLevel::Unknown | PressureLevel::Ok => false,
    }
}

/// Node ids where placing `model` makes sense (labels + static VRAM, not live headroom).
pub fn placement_eligible_node_ids(
    nodes: &[NodeSnapshot],
    model: &str,
    policy: &PolicyConfig,
    include_unhealthy: bool,
    avoid_ram_pressure: bool,
) -> Vec<NodeId> {
    let request_class = placement_class(model, policy);
    let mut candidates: Vec<&NodeSnapshot> = nodes
        .iter()
        .filter(|node| include_unhealthy || node.healthy)
        .filter(|node| label_ok(&node.labels, policy))
        .filter(|node| {
            !(avoid_ram_pressure
                && ram_pressure_blocks_placement(node, request_class, policy, true))
        })
        .filter(|node| static_capacity_fits(node, request_class, Some(model), policy))
        .collect();
    candidates.sort_by(|a, b| {
        capacity_preference(a, request_class)
            .partial_cmp(&capacity_preference(b, request_class))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    candidates.into_iter().map(|n| n.id.clone()).collect()
}

/// Resolve ensure targets per model.
///
/// Explicit ids are kept as-is (caller records `skipped_capacity` at run).
/// Unknown ids error. Callers must still validate explicit ids against `nodes`.
pub fn resolve_target_nodes(
    nodes: &[NodeSnapshot],
    models: &[String],
    spec: &TargetSpec,
    policy: &PolicyConfig,
    include_unhealthy: bool,
    avoid_ram_pressure: bool,
) -> Result<BTreeMap<String, Vec<NodeId>>, PlacementError> {
    match spec {
        TargetSpec::Placement => Ok(models
            .iter()
            .map(|model| {
                (
                    model.clone(),
                    placement_eligible_node_ids(
                        nodes,
                        model,
                        policy,
                        include_unhealthy,
                        avoid_ram_pressure,
                    ),
                )
            })
            .collect()),
        TargetSpec::All => {
            let all_ids: Vec<NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
            Ok(models
                .iter()
                .map(|model| (model.clone(), all_ids.clone()))
                .collect())
        }
        TargetSpec::Nodes(ids) => {
            let known: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
            for id in ids {
                if !known.contains(id.as_str()) {
                    return Err(PlacementError::UnknownNode(id.as_str().to_string()));
                }
            }
            Ok(models
                .iter()
                .map(|model| (model.clone(), ids.clone()))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Capacity, NodeConfig, RouterConfig};
    use crate::fleet::registry::Registry;

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    fn node(id: &str, vram: f64, gpus: u32) -> NodeConfig {
        NodeConfig {
            id: nid(id),
            url: Some(format!("http://{id}:11434")),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: Some(vram),
                ram_gb: Some(32.0),
                gpus: Some(gpus),
                cpu_cores: Some(8),
            },
            max_inflight: None,
            ssh: None,
            provision: None,
        }
    }

    fn fleet() -> (Registry, PolicyConfig) {
        let config = RouterConfig {
            nodes: vec![node("gpu", 80.0, 1), node("cpu", 0.0, 0)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("gpu"));
        registry.set_healthy(&nid("cpu"));
        (registry, config.policy)
    }

    #[test]
    fn star_places_embed_on_cpu() {
        let (registry, policy) = fleet();
        let nodes = registry.snapshot();
        let ids = placement_eligible_node_ids(&nodes, "qwen3-embedding:8b", &policy, false, false);
        assert!(ids.iter().any(|id| id.as_str() == "cpu"));
        assert!(ids.iter().any(|id| id.as_str() == "gpu"));
    }

    #[test]
    fn star_skips_large_on_cpu() {
        let (registry, policy) = fleet();
        let nodes = registry.snapshot();
        let ids = placement_eligible_node_ids(&nodes, "llama3.1:70b", &policy, false, false);
        assert_eq!(
            ids.iter().map(NodeId::as_str).collect::<Vec<_>>(),
            vec!["gpu"]
        );
    }

    #[test]
    fn hash_all_includes_cpu() {
        let (registry, policy) = fleet();
        let nodes = registry.snapshot();
        let resolved = resolve_target_nodes(
            &nodes,
            &["llama3.1:70b".into()],
            &TargetSpec::All,
            &policy,
            false,
            false,
        )
        .expect("resolve");
        let ids = &resolved["llama3.1:70b"];
        assert!(ids.iter().any(|id| id.as_str() == "cpu"));
        assert!(ids.iter().any(|id| id.as_str() == "gpu"));
    }

    #[test]
    fn unknown_explicit_id_errors() {
        let (registry, policy) = fleet();
        let nodes = registry.snapshot();
        let spec = TargetSpec::Nodes(vec![nid("nope")]);
        let err = resolve_target_nodes(&nodes, &["moondream".into()], &spec, &policy, false, false)
            .expect_err("unknown");
        assert!(matches!(err, PlacementError::UnknownNode(id) if id == "nope"));
    }

    #[test]
    fn ram_pressure_critical_blocks_sensitive() {
        let (registry, policy) = fleet();
        let mut node = registry.get(&nid("cpu")).expect("cpu");
        node.pressure_level = PressureLevel::Critical;
        let class = placement_class("qwen3-embedding:8b", &policy);
        assert!(ram_pressure_blocks_placement(&node, class, &policy, false));
        node.pressure_level = PressureLevel::Elevated;
        assert!(!ram_pressure_blocks_placement(&node, class, &policy, false));
        assert!(ram_pressure_blocks_placement(&node, class, &policy, true));
    }
}
