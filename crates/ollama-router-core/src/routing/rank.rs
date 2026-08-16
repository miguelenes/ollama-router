//! Utilization WLC + class preference over a registry snapshot.

use std::collections::HashSet;

use crate::config::PolicyConfig;
use crate::fleet::ids::NodeId;
use crate::fleet::registry::NodeSnapshot;

use super::classify::{parse_model_size_b, RequestClass};
use super::error::RoutingError;

const LARGE_VRAM_PER_B: f64 = 0.7;
const LARGE_VRAM_FLOOR: f64 = 2.0;
const LARGE_MIN_VRAM_FALLBACK: f64 = 16.0;

/// Ordered candidates plus the rejection reason when the list is empty.
#[derive(Clone, Debug)]
pub struct RankOutcome {
    pub ranked: Vec<NodeSnapshot>,
    pub reason: Option<RoutingError>,
    pub evaluated: Vec<NodeId>,
}

impl RankOutcome {
    pub fn ok(&self) -> bool {
        !self.ranked.is_empty()
    }

    pub fn rejection(&self) -> Option<RoutingError> {
        if self.ranked.is_empty() {
            self.reason
        } else {
            None
        }
    }
}

/// VRAM needed for a LARGE-class model (q4 rule of thumb).
pub fn estimate_large_vram_gb(model: Option<&str>) -> f64 {
    let Some(model) = model else {
        return LARGE_MIN_VRAM_FALLBACK;
    };
    match parse_model_size_b(model) {
        Some(size) => size * LARGE_VRAM_PER_B + LARGE_VRAM_FLOOR,
        None => LARGE_MIN_VRAM_FALLBACK,
    }
}

/// Admission-time VRAM estimate used for the reservation ledger.
pub fn estimate_request_vram_gb(
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> f64 {
    match request_class {
        RequestClass::Embed => policy.embed_reserve_vram_gb,
        RequestClass::Small => policy.small_reserve_vram_gb,
        RequestClass::Medium => {
            if let Some(model) = model {
                if let Some(size) = parse_model_size_b(model) {
                    let estimate = size * policy.vram_per_b + policy.vram_floor_gb;
                    return estimate.max(policy.medium_reserve_min_gb);
                }
            }
            policy.medium_reserve_min_gb
        }
        RequestClass::Large => estimate_large_vram_gb(model),
        RequestClass::Pull | RequestClass::Generic => 0.0,
    }
}

/// Estimate system RAM added by a cold request on a node.
pub fn estimate_request_ram_gb(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> f64 {
    match request_class {
        RequestClass::Embed => policy.embed_reserve_ram_gb,
        RequestClass::Small => policy.small_reserve_ram_gb,
        RequestClass::Medium | RequestClass::Large => {
            if node.vram_gb() <= 0.0 {
                estimate_request_vram_gb(request_class, model, policy)
            } else {
                policy.gpu_system_ram_overhead_gb
            }
        }
        RequestClass::Pull | RequestClass::Generic => 0.0,
    }
}

fn ram_sensitive(request_class: RequestClass, policy: &PolicyConfig) -> bool {
    request_class
        .as_policy_class()
        .is_some_and(|c| policy.ram_sensitive_classes.contains(&c))
}

fn reject_on_elevated(request_class: RequestClass, policy: &PolicyConfig) -> bool {
    request_class
        .as_policy_class()
        .is_some_and(|c| policy.reject_on_ram_elevated_for_classes.contains(&c))
}

/// RAM hard-filter. Returns Ok or Ram / RamPressure.
pub fn ram_fits(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> Result<(), RoutingError> {
    if !ram_sensitive(request_class, policy) {
        return Ok(());
    }
    if node.pressure_level == crate::fleet::registry::PressureLevel::Critical
        && policy.reject_on_ram_critical
    {
        return Err(RoutingError::RamPressure);
    }
    if node.pressure_level == crate::fleet::registry::PressureLevel::Elevated
        && reject_on_elevated(request_class, policy)
    {
        return Err(RoutingError::RamPressure);
    }
    let estimate = estimate_request_ram_gb(node, request_class, model, policy);
    if estimate <= 0.0 || node.ram_available_gb.is_none() {
        return Ok(());
    }
    if node.ram_gb() <= 0.0 {
        return Ok(());
    }
    let ram_available = node.ram_available_gb.unwrap_or(0.0);
    let projected = node.ram_gb() - ram_available + node.reserved_ram_gb + estimate;
    if projected > node.ram_gb() * policy.ram_headroom {
        return Err(RoutingError::Ram);
    }
    Ok(())
}

/// Static VRAM gate (no reservation ledger) — used by admission-wait.
pub fn static_capacity_fits(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> bool {
    match request_class {
        RequestClass::Embed | RequestClass::Small | RequestClass::Generic | RequestClass::Pull => {
            true
        }
        RequestClass::Medium => node
            .known_vram_gb()
            .is_some_and(|v| v >= policy.medium_min_vram_gb),
        RequestClass::Large => node
            .known_vram_gb()
            .is_some_and(|v| v >= estimate_large_vram_gb(model)),
    }
}

/// Static + live VRAM gate (no RAM). Used to distinguish Capacity vs Ram misses.
pub fn vram_fits(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> bool {
    if matches!(request_class, RequestClass::Generic | RequestClass::Pull) {
        return true;
    }
    if request_class == RequestClass::Embed && policy.embed_reserve_vram_gb <= 0.0 {
        return true;
    }
    if request_class == RequestClass::Small && policy.small_reserve_vram_gb <= 0.0 {
        return true;
    }
    if request_class == RequestClass::Medium {
        match node.known_vram_gb() {
            Some(v) if v >= policy.medium_min_vram_gb => {}
            _ => return false,
        }
    }
    if request_class == RequestClass::Large {
        match node.known_vram_gb() {
            Some(v) if v >= estimate_large_vram_gb(model) => {}
            _ => return false,
        }
    }

    let request_estimate = estimate_request_vram_gb(request_class, model, policy);
    if request_estimate <= 0.0 && node.reserved_vram_gb <= 0.0 {
        return true;
    }

    if let Some(vram) = node.known_vram_gb() {
        if vram > 0.0 {
            if let Some(loaded) = node.loaded_vram_gb {
                let projected = loaded + node.reserved_vram_gb + request_estimate;
                let limit = vram * policy.vram_headroom;
                if projected > limit {
                    return false;
                }
            } else if node.reserved_vram_gb > 0.0 {
                let projected = node.reserved_vram_gb + request_estimate;
                let limit = vram * policy.vram_headroom;
                if projected > limit {
                    return false;
                }
            }
        }
    }

    true
}

/// Capacity gate using effective inventory plus live VRAM pressure and RAM.
pub fn capacity_fits(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> bool {
    vram_fits(node, request_class, model, policy)
        && ram_fits(node, request_class, model, policy).is_ok()
}

pub(crate) fn label_ok(labels: &[String], policy: &PolicyConfig) -> bool {
    if !policy.must_have_labels.is_empty() {
        let have: HashSet<&str> = labels.iter().map(String::as_str).collect();
        if !policy
            .must_have_labels
            .iter()
            .all(|need| have.contains(need.as_str()))
        {
            return false;
        }
    }
    if !policy.avoid_labels.is_empty() {
        let have: HashSet<&str> = labels.iter().map(String::as_str).collect();
        if policy
            .avoid_labels
            .iter()
            .any(|avoid| have.contains(avoid.as_str()))
        {
            return false;
        }
    }
    true
}

/// Preference for unknown VRAM so it sorts after any known card but before CPU.
const UNKNOWN_VRAM_PREFERENCE: f64 = 1_000.0;
const KNOWN_CPU_PREFERENCE_BASE: f64 = 2_000.0;

pub(crate) fn capacity_preference(node: &NodeSnapshot, request_class: RequestClass) -> f64 {
    match request_class {
        RequestClass::Embed => {
            let mut base = node.known_vram_gb().unwrap_or(UNKNOWN_VRAM_PREFERENCE);
            if let (Some(vram), Some(loaded)) = (node.known_vram_gb(), node.loaded_vram_gb) {
                if vram > 0.0 && vram - loaded < 2.0 {
                    base += 100.0;
                }
            }
            base
        }
        RequestClass::Large => -node.known_vram_gb().unwrap_or(0.0),
        RequestClass::Small => match node.known_gpus() {
            Some(gpus) if gpus >= 1 => node.known_vram_gb().unwrap_or(0.0),
            None => UNKNOWN_VRAM_PREFERENCE,
            Some(_) => KNOWN_CPU_PREFERENCE_BASE + node.known_vram_gb().unwrap_or(0.0),
        },
        RequestClass::Medium | RequestClass::Generic | RequestClass::Pull => {
            node.known_vram_gb().unwrap_or(UNKNOWN_VRAM_PREFERENCE)
        }
    }
}

/// Sort key (lower is better). Sticky affinity may promote only on exact equality.
pub fn load_key(
    node: &NodeSnapshot,
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> (f64, f64, f64, f64) {
    let warm = if policy.prefer_warm_models {
        if let Some(model) = model {
            if node.has_model_loaded(model) {
                0.0
            } else {
                1.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };
    let pressure_penalty = match node.pressure_level {
        crate::fleet::registry::PressureLevel::Critical => policy.ram_critical_score_penalty,
        crate::fleet::registry::PressureLevel::Elevated => policy.ram_elevated_score_penalty,
        crate::fleet::registry::PressureLevel::Unknown
        | crate::fleet::registry::PressureLevel::Ok => 0.0,
    };
    let available = -(node.ram_available_ratio.unwrap_or(0.0));
    let preference = capacity_preference(node, request_class);
    let base_cap = node.base_inflight_cap(policy.default_max_inflight).max(1);
    let utilization = f64::from(node.inflight) / f64::from(base_cap);
    (
        utilization * policy.inflight_weight,
        pressure_penalty,
        preference,
        warm + available * 0.001,
    )
}

fn snapshot_ids<'a>(nodes: impl IntoIterator<Item = &'a NodeSnapshot>) -> Vec<NodeId> {
    nodes.into_iter().map(|n| n.id.clone()).collect()
}

/// Ordered candidate list (best first), rejection reason, evaluated ids.
pub fn rank_nodes(
    nodes: &[NodeSnapshot],
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
    sticky_owner: Option<&NodeId>,
    excluded_node_ids: &HashSet<NodeId>,
    tie_break: u64,
) -> RankOutcome {
    if nodes.is_empty() {
        return RankOutcome {
            ranked: Vec::new(),
            reason: Some(RoutingError::NoNodes),
            evaluated: Vec::new(),
        };
    }

    let healthy: Vec<&NodeSnapshot> = nodes
        .iter()
        .filter(|n| n.healthy && !n.draining && !excluded_node_ids.contains(&n.id))
        .collect();
    if healthy.is_empty() {
        return RankOutcome {
            ranked: Vec::new(),
            reason: Some(RoutingError::NoHealthy),
            evaluated: snapshot_ids(nodes),
        };
    }

    let candidates: Vec<&NodeSnapshot> = if let Some(model) = model {
        let with_model: Vec<&NodeSnapshot> = healthy
            .iter()
            .copied()
            .filter(|n| n.has_model(model))
            .collect();
        if with_model.is_empty() {
            return RankOutcome {
                ranked: Vec::new(),
                reason: Some(RoutingError::ModelMissing),
                evaluated: snapshot_ids(healthy),
            };
        }
        with_model
    } else {
        healthy
    };

    let labeled: Vec<&NodeSnapshot> = candidates
        .iter()
        .copied()
        .filter(|n| label_ok(&n.labels, policy))
        .collect();

    let fitting: Vec<&NodeSnapshot> = labeled
        .iter()
        .copied()
        .filter(|n| capacity_fits(n, request_class, model, policy))
        .collect();
    if fitting.is_empty() {
        let any_vram = labeled
            .iter()
            .any(|n| vram_fits(n, request_class, model, policy));
        if !any_vram {
            return RankOutcome {
                ranked: Vec::new(),
                reason: Some(RoutingError::Capacity),
                evaluated: snapshot_ids(labeled),
            };
        }
        let mut reason = RoutingError::Ram;
        for node in &labeled {
            if !vram_fits(node, request_class, model, policy) {
                continue;
            }
            if let Err(r) = ram_fits(node, request_class, model, policy) {
                if r == RoutingError::RamPressure {
                    reason = RoutingError::RamPressure;
                    break;
                }
                reason = RoutingError::Ram;
            }
        }
        return RankOutcome {
            ranked: Vec::new(),
            reason: Some(reason),
            evaluated: snapshot_ids(labeled),
        };
    }

    let uncapped: Vec<&NodeSnapshot> = fitting
        .iter()
        .copied()
        .filter(|n| !n.is_saturated(policy.default_max_inflight))
        .collect();
    if uncapped.is_empty() {
        return RankOutcome {
            ranked: Vec::new(),
            reason: Some(RoutingError::Saturated),
            evaluated: snapshot_ids(fitting),
        };
    }

    let mut order: Vec<usize> = (0..uncapped.len()).collect();
    order.sort_by(|&i, &j| {
        let a = uncapped[i];
        let b = uncapped[j];
        let ka = load_key(a, request_class, model, policy);
        let kb = load_key(b, request_class, model, policy);
        ka.partial_cmp(&kb)
            // NaN keys are rejected at config/agent ingest; Equal is defensive.
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });

    let best_key = load_key(uncapped[order[0]], request_class, model, policy);
    if let Some(owner_id) = sticky_owner {
        if let Some(pos) = order.iter().position(|&i| uncapped[i].id == *owner_id) {
            if pos != 0 && load_key(uncapped[order[pos]], request_class, model, policy) == best_key
            {
                order[..=pos].rotate_right(1);
            }
        }
    }

    let tied_len = order
        .iter()
        .filter(|&&i| load_key(uncapped[i], request_class, model, policy) == best_key)
        .count();
    if tied_len > 1 && tie_break > 0 {
        let rot = (tie_break as usize) % tied_len;
        order[..tied_len].rotate_left(rot);
    }

    let ranked: Vec<NodeSnapshot> = order.iter().map(|&i| uncapped[i].clone()).collect();
    let evaluated = snapshot_ids(&ranked);
    RankOutcome {
        ranked,
        reason: None,
        evaluated,
    }
}

/// True when `insufficient_capacity` would lift if the reservation ledger were empty.
pub fn blocked_only_by_reservations(
    nodes: &[NodeSnapshot],
    request_class: RequestClass,
    model: Option<&str>,
    policy: &PolicyConfig,
) -> bool {
    for node in nodes {
        if !node.healthy || node.draining {
            continue;
        }
        if let Some(model) = model {
            if !node.has_model(model) {
                continue;
            }
        }
        if !label_ok(&node.labels, policy) {
            continue;
        }
        if capacity_fits(node, request_class, model, policy) {
            continue;
        }
        let mut cleared = node.clone();
        cleared.reserved_vram_gb = 0.0;
        cleared.reserved_ram_gb = 0.0;
        if capacity_fits(&cleared, request_class, model, policy) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Capacity, NodeConfig, RouterConfig};
    use crate::fleet::registry::{suggested_max_inflight, Registry};
    use proptest::prelude::*;

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    fn node(id: &str, vram: f64, gpus: u32, max_inflight: Option<u32>) -> NodeConfig {
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
            max_inflight,
        }
    }

    fn node_unknown(id: &str, max_inflight: Option<u32>) -> NodeConfig {
        NodeConfig {
            id: nid(id),
            url: Some(format!("http://{id}:11434")),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: None,
                ram_gb: Some(32.0),
                gpus: None,
                cpu_cores: Some(8),
            },
            max_inflight,
        }
    }

    fn fleet() -> (Registry, PolicyConfig) {
        let config = RouterConfig {
            nodes: vec![
                node("node-a", 8.0, 1, None),
                node("node-b", 0.0, 0, None),
                node("node-c", 24.0, 1, None),
            ],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        for id in ["node-a", "node-b", "node-c"] {
            registry.set_healthy(&nid(id));
        }
        registry.update_models(&nid("node-a"), ["qwen3-embedding:8b", "llama3.2:3b"]);
        registry.update_models(
            &nid("node-b"),
            ["qwen3-embedding:0.6b", "moondream", "llama3.2:3b"],
        );
        registry.update_models(
            &nid("node-c"),
            ["qwen3-embedding:8b", "llama3.2:3b", "llama3.1:70b"],
        );
        (registry, config.policy)
    }

    #[test]
    fn no_healthy_nodes() {
        let config = RouterConfig {
            nodes: vec![node("node-a", 8.0, 1, None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::NoHealthy));
    }

    #[test]
    fn saturated_never_selected() {
        let a = nid("node-a");
        let c = nid("node-c");
        let config = RouterConfig {
            nodes: vec![
                node("node-a", 8.0, 1, Some(1)),
                node("node-b", 0.0, 0, None),
                node("node-c", 24.0, 1, Some(1)),
            ],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&a);
        registry.set_healthy(&c);
        registry.update_models(&a, ["qwen3-embedding:8b"]);
        registry.update_models(&c, ["qwen3-embedding:8b"]);
        registry.inflight_inc(&a);
        registry.inflight_inc(&c);
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::Saturated));
    }

    #[test]
    fn embed_prefers_smaller_gpu() {
        let (registry, policy) = fleet();
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "node-a");
    }

    #[test]
    fn sticky_does_not_override_better_load_key() {
        let (registry, mut policy) = fleet();
        policy.sticky_affinity = true;
        registry.update_ps_state(&nid("node-c"), ["qwen3-embedding:8b"], Some(5.0));
        let owner = registry.sticky_owner("qwen3-embedding:8b");
        assert_eq!(owner.as_ref().map(|id| id.as_str()), Some("node-c"));
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &policy,
            owner.as_ref(),
            &HashSet::new(),
            0,
        );
        assert_eq!(outcome.ranked[0].id.as_str(), "node-a");
    }

    #[test]
    fn retry_exclusion_skips_attempted_id() {
        let (registry, policy) = fleet();
        let mut excluded = HashSet::new();
        excluded.insert(nid("node-a"));
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &policy,
            None,
            &excluded,
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "node-c");
        assert!(outcome.ranked.iter().all(|n| n.id.as_str() != "node-a"));
    }

    #[test]
    fn large_model_insufficient_capacity() {
        let (registry, policy) = fleet();
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Large,
            Some("llama3.1:70b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::Capacity));
    }

    #[test]
    fn suggested_max_inflight_reexport() {
        assert_eq!(suggested_max_inflight(8.0), 2);
    }

    #[test]
    fn demand_scale_allowlist() {
        assert!(RoutingError::NoNodes.requests_demand_scale_up());
        assert!(RoutingError::NoHealthy.requests_demand_scale_up());
        assert!(RoutingError::Saturated.requests_demand_scale_up());
        assert!(RoutingError::Capacity.requests_demand_scale_up());
        assert!(!RoutingError::ModelMissing.requests_demand_scale_up());
        assert!(!RoutingError::Ram.requests_demand_scale_up());
        assert!(!RoutingError::RamPressure.requests_demand_scale_up());
    }

    #[test]
    fn blocked_only_by_reservations_false_when_loaded_vram_fills_headroom() {
        let config = RouterConfig {
            nodes: vec![node("gpu", 24.0, 1, None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("gpu");
        registry.set_healthy(&id);
        registry.update_models(&id, ["llama3.1:8b"]);
        let mut snap = registry.snapshot();
        snap[0].loaded_vram_gb = Some(20.0);
        snap[0].reserved_vram_gb = 0.0;
        assert!(!blocked_only_by_reservations(
            &snap,
            RequestClass::Medium,
            Some("llama3.1:8b"),
            &config.policy,
        ));
    }

    #[test]
    fn blocked_only_by_reservations_true_when_ledger_is_the_blocker() {
        let config = RouterConfig {
            nodes: vec![node("gpu", 24.0, 1, None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("gpu");
        registry.set_healthy(&id);
        registry.update_models(&id, ["llama3.1:8b"]);
        let mut snap = registry.snapshot();
        snap[0].loaded_vram_gb = Some(10.0);
        snap[0].reserved_vram_gb = 12.0;
        assert!(blocked_only_by_reservations(
            &snap,
            RequestClass::Medium,
            Some("llama3.1:8b"),
            &config.policy,
        ));
    }

    #[test]
    fn blocked_only_by_reservations_false_for_undersized_large() {
        let (registry, policy) = fleet();
        assert!(!blocked_only_by_reservations(
            &registry.snapshot(),
            RequestClass::Large,
            Some("llama3.1:70b"),
            &policy,
        ));
    }

    #[test]
    fn nan_load_key_falls_back_to_id_order() {
        let config = RouterConfig {
            nodes: vec![node("node-b", 8.0, 1, None), node("node-a", 8.0, 1, None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("node-a"));
        registry.set_healthy(&nid("node-b"));
        registry.update_models(&nid("node-a"), ["llama3.2:3b"]);
        registry.update_models(&nid("node-b"), ["llama3.2:3b"]);
        let mut snap = registry.snapshot();
        for node in &mut snap {
            node.ram_available_ratio = Some(f64::NAN);
        }
        let outcome = rank_nodes(
            &snap,
            RequestClass::Small,
            Some("llama3.2:3b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert_eq!(outcome.ranked[0].id.as_str(), "node-a");
        assert_eq!(outcome.ranked[1].id.as_str(), "node-b");
    }

    #[test]
    fn ram_pressure_on_fitting_vram_does_not_demand_scale() {
        let config = RouterConfig {
            nodes: vec![node("gpu", 48.0, 1, None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("gpu");
        registry.set_healthy(&id);
        registry.update_models(&id, ["llama3.1:8b"]);
        registry.set_pressure_level(&id, crate::fleet::registry::PressureLevel::Critical);
        let mut snap = registry.snapshot();
        snap[0].ram_available_gb = Some(0.5);
        snap[0].ram_available_ratio = Some(0.01);
        let outcome = rank_nodes(
            &snap,
            RequestClass::Large,
            Some("llama3.1:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::RamPressure));
        assert!(!outcome.reason.unwrap().requests_demand_scale_up());
    }

    fn fleet_from_yaml(yaml: &str) -> (Registry, PolicyConfig) {
        let nodes = crate::fleet::parse_fleet_yaml(yaml, "test-fleet.yaml").expect("fleet yaml");
        let config = RouterConfig {
            nodes,
            ..Default::default()
        };
        let registry = Registry::new(&config);
        for node in registry.snapshot() {
            registry.set_healthy(&node.id);
        }
        (registry, config.policy)
    }

    const MIXED_FLEET: &str = r#"
version: 1
nodes:
  - id: cpu
    url: http://127.0.0.1:11434
    capacity:
      vram_gb: 0
      ram_gb: 32
      gpus: 0
      cpu_cores: 8
  - id: gpu8
    url: http://127.0.0.1:11435
    capacity:
      vram_gb: 8
      ram_gb: 32
      gpus: 1
      cpu_cores: 8
  - id: gpu24
    url: http://127.0.0.1:11436
    capacity:
      vram_gb: 24
      ram_gb: 64
      gpus: 1
      cpu_cores: 16
"#;

    #[test]
    fn cpu_only_selected_for_embed() {
        let yaml = r#"
version: 1
nodes:
  - id: cpu
    url: http://127.0.0.1:11434
    capacity:
      vram_gb: 0
      ram_gb: 32
      gpus: 0
      cpu_cores: 8
"#;
        let (registry, policy) = fleet_from_yaml(yaml);
        registry.update_models(&nid("cpu"), ["qwen3-embedding:0.6b"]);
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:0.6b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "cpu");
        assert_eq!(outcome.ranked[0].vram_gb(), 0.0);
    }

    #[test]
    fn small_prefers_8gib_gpu_over_cpu() {
        let (registry, policy) = fleet_from_yaml(MIXED_FLEET);
        for id in ["cpu", "gpu8", "gpu24"] {
            registry.update_models(&nid(id), ["llama3.2:3b"]);
        }
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Small,
            Some("llama3.2:3b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "gpu8");
    }

    #[test]
    fn large_prefers_bigger_vram_and_skips_undersized() {
        let (registry, policy) = fleet_from_yaml(MIXED_FLEET);
        for id in ["cpu", "gpu8", "gpu24"] {
            registry.update_models(&nid(id), ["llama3.1:70b"]);
        }
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Large,
            Some("llama3.1:70b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::Capacity));

        registry.update_models(&nid("gpu24"), ["llama3.1:8b"]);
        let mid = rank_nodes(
            &registry.snapshot(),
            RequestClass::Large,
            Some("llama3.1:8b"),
            &policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(mid.ok());
        assert_eq!(mid.ranked[0].id.as_str(), "gpu24");
        assert!(mid.ranked.iter().all(|n| n.id.as_str() != "cpu"));
    }

    #[test]
    fn embed_utilization_gap_beats_class_preference() {
        let config = RouterConfig {
            nodes: vec![
                node("gpu48", 48.0, 1, Some(8)),
                node("cpu", 0.0, 0, Some(2)),
            ],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("gpu48"));
        registry.set_healthy(&nid("cpu"));
        registry.update_models(&nid("gpu48"), ["qwen3-embedding:8b"]);
        registry.update_models(&nid("cpu"), ["qwen3-embedding:8b"]);
        for _ in 0..2 {
            registry.inflight_inc(&nid("gpu48"));
        }
        registry.inflight_inc(&nid("cpu"));
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert_eq!(outcome.ranked[0].id.as_str(), "gpu48");
    }

    #[test]
    fn small_prefers_known_gpu_then_unknown_then_cpu() {
        let config = RouterConfig {
            nodes: vec![
                node("gpu", 8.0, 1, None),
                node_unknown("unknown", None),
                node("cpu", 0.0, 0, None),
            ],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        for id in ["gpu", "unknown", "cpu"] {
            registry.set_healthy(&nid(id));
            registry.update_models(&nid(id), ["llama3.2:3b"]);
        }
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Small,
            Some("llama3.2:3b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        let ids: Vec<&str> = outcome.ranked.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["gpu", "unknown", "cpu"]);
    }

    #[test]
    fn embed_prefers_known_8gib_over_unknown() {
        let config = RouterConfig {
            nodes: vec![node("gpu8", 8.0, 1, None), node_unknown("unknown", None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        for id in ["gpu8", "unknown"] {
            registry.set_healthy(&nid(id));
            registry.update_models(&nid(id), ["qwen3-embedding:8b"]);
        }
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Embed,
            Some("qwen3-embedding:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "gpu8");
    }

    #[test]
    fn medium_prefers_known_8gib_over_unknown() {
        let config = RouterConfig {
            nodes: vec![node("gpu8", 8.0, 1, None), node_unknown("unknown", None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        for id in ["gpu8", "unknown"] {
            registry.set_healthy(&nid(id));
            registry.update_models(&nid(id), ["llama3.1:8b"]);
        }
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Medium,
            Some("llama3.1:8b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "gpu8");
        assert!(outcome.ranked.iter().all(|n| n.id.as_str() != "unknown"));
    }

    #[test]
    fn large_unknown_vram_does_not_fit() {
        let config = RouterConfig {
            nodes: vec![node_unknown("unknown", None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("unknown"));
        registry.update_models(&nid("unknown"), ["llama3.1:70b"]);
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Large,
            Some("llama3.1:70b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(!outcome.ok());
        assert_eq!(outcome.reason, Some(RoutingError::Capacity));
    }

    #[test]
    fn small_still_forwards_to_unknown_vram() {
        let config = RouterConfig {
            nodes: vec![node_unknown("unknown", None)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("unknown"));
        registry.update_models(&nid("unknown"), ["llama3.2:3b"]);
        let outcome = rank_nodes(
            &registry.snapshot(),
            RequestClass::Small,
            Some("llama3.2:3b"),
            &config.policy,
            None,
            &HashSet::new(),
            0,
        );
        assert!(outcome.ok());
        assert_eq!(outcome.ranked[0].id.as_str(), "unknown");
        assert!(outcome.ranked[0].known_vram_gb().is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn utilization_gap_beats_embed_preference(
            gpu_inflight in 0u32..=2,
            cpu_inflight in 1u32..=2,
        ) {
            let gpu_util = f64::from(gpu_inflight) / 8.0;
            let cpu_util = f64::from(cpu_inflight) / 2.0;
            prop_assume!(gpu_util + 1e-9 < cpu_util);

            let config = RouterConfig {
                nodes: vec![
                    node("gpu48", 48.0, 1, Some(8)),
                    node("cpu", 0.0, 0, Some(2)),
                ],
                ..Default::default()
            };
            let registry = Registry::new(&config);
            registry.set_healthy(&nid("gpu48"));
            registry.set_healthy(&nid("cpu"));
            registry.update_models(&nid("gpu48"), ["qwen3-embedding:8b"]);
            registry.update_models(&nid("cpu"), ["qwen3-embedding:8b"]);
            for _ in 0..gpu_inflight {
                registry.inflight_inc(&nid("gpu48"));
            }
            for _ in 0..cpu_inflight {
                registry.inflight_inc(&nid("cpu"));
            }
            let outcome = rank_nodes(
                &registry.snapshot(),
                RequestClass::Embed,
                Some("qwen3-embedding:8b"),
                &config.policy,
                None,
                &HashSet::new(),
                0,
            );
            prop_assert!(outcome.ok());
            prop_assert_eq!(outcome.ranked[0].id.as_str(), "gpu48");
        }
    }
}
