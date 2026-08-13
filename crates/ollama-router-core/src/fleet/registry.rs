//! Live in-memory fleet view: health, inflight, models, reservations.

use std::collections::{HashMap, HashSet};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::SystemTime;

use crate::config::{Capacity, HealthConfig, NodeConfig, PolicyConfig, RouterConfig};
use crate::fleet::ids::NodeId;

/// RAM pressure classification used by scoring and hard filters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PressureLevel {
    #[default]
    Unknown,
    Ok,
    Elevated,
    Critical,
}

impl PressureLevel {
    /// Python-stable token (`unknown` / `elevated` / `critical`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ok => "ok",
            Self::Elevated => "elevated",
            Self::Critical => "critical",
        }
    }
}

/// Lowercase + trim; Ollama model names are case-insensitive.
pub fn normalize_model(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Base name without the tag: `llama3.2:1b` → `llama3.2`.
pub fn model_base(normalized: &str) -> &str {
    normalized
        .split_once(':')
        .map(|(b, _)| b)
        .unwrap_or(normalized)
}

/// VRAM-tier concurrency suggestion when no explicit cap is set.
pub fn suggested_max_inflight(vram_gb: f64) -> u32 {
    if vram_gb < 12.0 {
        2
    } else if vram_gb < 24.0 {
        3
    } else if vram_gb < 48.0 {
        4
    } else {
        8
    }
}

fn models_match(have: &HashSet<String>, requested: &str) -> bool {
    let target = normalize_model(requested);
    if have.contains(&target) {
        return true;
    }
    if !target.contains(':') {
        return have.iter().any(|m| model_base(m) == target);
    }
    false
}

/// Ranking / forwarding snapshot of one node. Pure; no locks.
#[derive(Clone, Debug)]
pub struct NodeSnapshot {
    pub id: NodeId,
    pub url: Option<String>,
    pub labels: Vec<String>,
    pub healthy: bool,
    pub models: HashSet<String>,
    pub loaded_models: HashSet<String>,
    pub inflight: u32,
    pub max_inflight: Option<u32>,
    pub capacity: Capacity,
    pub reserved_vram_gb: f64,
    pub reserved_ram_gb: f64,
    pub loaded_vram_gb: Option<f64>,
    pub ram_available_gb: Option<f64>,
    pub ram_available_ratio: Option<f64>,
    pub pressure_level: PressureLevel,
    pub fail_streak: u32,
}

impl NodeSnapshot {
    /// Exact normalized match, or untagged base-name match.
    pub fn has_model(&self, model: &str) -> bool {
        models_match(&self.models, model)
    }

    /// Whether `/api/ps` currently shows the model loaded.
    pub fn has_model_loaded(&self, model: &str) -> bool {
        models_match(&self.loaded_models, model)
    }

    /// Effective VRAM (0 when unset).
    pub fn vram_gb(&self) -> f64 {
        self.capacity.vram_gb()
    }

    /// Effective RAM (0 when unset).
    pub fn ram_gb(&self) -> f64 {
        self.capacity.ram_gb()
    }

    /// Effective GPU count.
    pub fn gpus(&self) -> u32 {
        self.capacity.gpus()
    }

    /// Concurrency ceiling before pressure derating.
    pub fn base_inflight_cap(&self, default_max_inflight: Option<u32>) -> u32 {
        if let Some(max) = self.max_inflight {
            return max;
        }
        if let Some(max) = default_max_inflight {
            return max;
        }
        suggested_max_inflight(self.vram_gb())
    }

    /// Pressure-aware cap used for the saturation hard filter.
    pub fn max_inflight_effective(&self, default_max_inflight: Option<u32>) -> u32 {
        if let Some(max) = self.max_inflight {
            return max;
        }
        if let Some(max) = default_max_inflight {
            return max;
        }
        let mut effective = suggested_max_inflight(self.vram_gb());
        match self.pressure_level {
            PressureLevel::Elevated => effective = effective.saturating_sub(1).max(1),
            PressureLevel::Critical => effective = 1,
            PressureLevel::Unknown | PressureLevel::Ok => {}
        }
        effective
    }

    /// True when inflight has reached the effective cap.
    pub fn is_saturated(&self, default_max_inflight: Option<u32>) -> bool {
        self.inflight >= self.max_inflight_effective(default_max_inflight)
    }
}

struct NodeState {
    config: NodeConfig,
    healthy: bool,
    fail_streak: u32,
    success_streak: u32,
    models: HashSet<String>,
    loaded_models: HashSet<String>,
    inflight: u32,
    last_client_request_at: Option<SystemTime>,
    loaded_vram_gb: Option<f64>,
    reserved_vram_gb: f64,
    reserved_ram_gb: f64,
    reservations: HashMap<u64, (String, f64)>,
    ram_reservations: HashMap<u64, (String, f64)>,
    next_reservation_id: u64,
    ram_available_gb: Option<f64>,
    ram_available_ratio: Option<f64>,
    pressure_level: PressureLevel,
    capacity_effective: Capacity,
}

impl NodeState {
    fn from_config(config: NodeConfig) -> Self {
        let capacity_effective = config.static_capacity.clone();
        Self {
            config,
            healthy: false,
            fail_streak: 0,
            success_streak: 0,
            models: HashSet::new(),
            loaded_models: HashSet::new(),
            inflight: 0,
            last_client_request_at: None,
            loaded_vram_gb: None,
            reserved_vram_gb: 0.0,
            reserved_ram_gb: 0.0,
            reservations: HashMap::new(),
            ram_reservations: HashMap::new(),
            next_reservation_id: 0,
            ram_available_gb: None,
            ram_available_ratio: None,
            pressure_level: PressureLevel::Unknown,
            capacity_effective,
        }
    }

    fn snapshot(&self) -> NodeSnapshot {
        NodeSnapshot {
            id: self.config.id.clone(),
            url: self.config.url.clone(),
            labels: self.config.labels.clone(),
            healthy: self.healthy,
            models: self.models.clone(),
            loaded_models: self.loaded_models.clone(),
            inflight: self.inflight,
            max_inflight: self.config.max_inflight,
            capacity: self.capacity_effective.clone(),
            reserved_vram_gb: self.reserved_vram_gb,
            reserved_ram_gb: self.reserved_ram_gb,
            loaded_vram_gb: self.loaded_vram_gb,
            ram_available_gb: self.ram_available_gb,
            ram_available_ratio: self.ram_available_ratio,
            pressure_level: self.pressure_level,
            fail_streak: self.fail_streak,
        }
    }
}

struct Inner {
    nodes: HashMap<NodeId, NodeState>,
}

/// Live fleet registry. Safe to share across Axum tasks (`Arc<Registry>`).
pub struct Registry {
    inner: RwLock<Inner>,
    health: HealthConfig,
    policy: PolicyConfig,
}

impl Registry {
    /// Build from configured nodes. All start unhealthy.
    pub fn new(config: &RouterConfig) -> Self {
        let nodes = config
            .nodes
            .iter()
            .map(|n| (n.id.clone(), NodeState::from_config(n.clone())))
            .collect();
        Self {
            inner: RwLock::new(Inner { nodes }),
            health: config.health.clone(),
            policy: config.policy.clone(),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Inner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Policy snapshot used by ranking.
    pub fn policy(&self) -> &PolicyConfig {
        &self.policy
    }

    /// Health knobs (fail credits).
    pub fn health(&self) -> &HealthConfig {
        &self.health
    }

    /// Ranking snapshot of every configured node.
    pub fn snapshot(&self) -> Vec<NodeSnapshot> {
        self.read()
            .nodes
            .values()
            .map(NodeState::snapshot)
            .collect()
    }

    /// Snapshot of one node.
    pub fn get(&self, id: &NodeId) -> Option<NodeSnapshot> {
        self.read().nodes.get(id).map(NodeState::snapshot)
    }

    /// Mark a node healthy (tests and the future health checker).
    pub fn set_healthy(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.healthy = true;
            node.fail_streak = 0;
            node.success_streak = 0;
        }
    }

    /// Mark a node unhealthy.
    pub fn set_unhealthy(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.healthy = false;
            node.success_streak = 0;
        }
    }

    /// Replace the on-disk model set reported by `/api/tags`.
    pub fn update_models(&self, id: &NodeId, models: impl IntoIterator<Item = impl AsRef<str>>) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.models = models
                .into_iter()
                .map(|m| normalize_model(m.as_ref()))
                .collect();
        }
    }

    /// Record live `/api/ps` state (tests inject warmth).
    pub fn update_ps_state(
        &self,
        id: &NodeId,
        loaded_models: impl IntoIterator<Item = impl AsRef<str>>,
        loaded_vram_gb: Option<f64>,
    ) {
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        node.loaded_models = loaded_models
            .into_iter()
            .map(|m| normalize_model(m.as_ref()))
            .collect();
        node.loaded_vram_gb = loaded_vram_gb;
        reconcile_ps_reservations(node);
    }

    /// Admit a client generate/chat/embed forward. Writes `last_client_request_at`.
    pub fn inflight_inc(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_add(1);
            node.last_client_request_at = Some(SystemTime::now());
        }
    }

    /// Release one inflight slot.
    pub fn inflight_dec(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_sub(1);
        }
    }

    /// Wall-clock of last client generate/chat/embed (idle scale-down).
    pub fn last_client_request_at(&self, id: &NodeId) -> Option<SystemTime> {
        self.read()
            .nodes
            .get(id)
            .and_then(|n| n.last_client_request_at)
    }

    /// Current inflight count.
    pub fn inflight(&self, id: &NodeId) -> u32 {
        self.read().nodes.get(id).map(|n| n.inflight).unwrap_or(0)
    }

    /// Current fail streak.
    pub fn fail_streak(&self, id: &NodeId) -> u32 {
        self.read()
            .nodes
            .get(id)
            .map(|n| n.fail_streak)
            .unwrap_or(0)
    }

    /// Current reserved VRAM.
    pub fn reserved_vram_gb(&self, id: &NodeId) -> f64 {
        self.read()
            .nodes
            .get(id)
            .map(|n| n.reserved_vram_gb)
            .unwrap_or(0.0)
    }

    /// Clear the fail streak after a successful upstream response.
    pub fn mark_request_success(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.fail_streak = 0;
        }
    }

    /// Credit a dying-node failure toward the health streak.
    pub fn mark_request_failure(&self, id: &NodeId) {
        self.credit_fail_streak(id, self.health.request_fail_credit);
    }

    /// Lighter streak credit for pre-body 429/503.
    pub fn mark_request_overload(&self, id: &NodeId) {
        self.credit_fail_streak(id, self.health.overload_fail_credit);
    }

    fn credit_fail_streak(&self, id: &NodeId, credit: u32) {
        if credit == 0 {
            return;
        }
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        node.fail_streak = node.fail_streak.saturating_add(credit);
        if node.healthy && node.fail_streak >= self.health.fail_streak_threshold {
            node.healthy = false;
            node.success_streak = 0;
        }
    }

    /// Reserve VRAM for a cold load. `None` when estimate is 0 or the node is unknown.
    pub fn reserve_vram(&self, id: &NodeId, model: &str, estimate_gb: f64) -> Option<u64> {
        if estimate_gb <= 0.0 {
            return None;
        }
        let mut inner = self.write();
        let node = inner.nodes.get_mut(id)?;
        let reservation_id = node.next_reservation_id;
        node.next_reservation_id = node.next_reservation_id.saturating_add(1);
        node.reservations
            .insert(reservation_id, (normalize_model(model), estimate_gb));
        node.reserved_vram_gb += estimate_gb;
        Some(reservation_id)
    }

    /// Release a VRAM reservation (no-op for `None`).
    pub fn release_vram(&self, id: &NodeId, reservation_id: Option<u64>) {
        let Some(reservation_id) = reservation_id else {
            return;
        };
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        if let Some((_, estimate)) = node.reservations.remove(&reservation_id) {
            node.reserved_vram_gb = (node.reserved_vram_gb - estimate).max(0.0);
        }
    }

    /// Reserve system RAM for a cold load.
    pub fn reserve_ram(&self, id: &NodeId, model: &str, estimate_gb: f64) -> Option<u64> {
        if estimate_gb <= 0.0 {
            return None;
        }
        let mut inner = self.write();
        let node = inner.nodes.get_mut(id)?;
        let reservation_id = node.next_reservation_id;
        node.next_reservation_id = node.next_reservation_id.saturating_add(1);
        node.ram_reservations
            .insert(reservation_id, (normalize_model(model), estimate_gb));
        node.reserved_ram_gb += estimate_gb;
        Some(reservation_id)
    }

    /// Release a RAM reservation (no-op for `None`).
    pub fn release_ram(&self, id: &NodeId, reservation_id: Option<u64>) {
        let Some(reservation_id) = reservation_id else {
            return;
        };
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        if let Some((_, estimate)) = node.ram_reservations.remove(&reservation_id) {
            node.reserved_ram_gb = (node.reserved_ram_gb - estimate).max(0.0);
        }
    }

    /// Sticky-affinity owner: prefer a warm healthy holder of `model`.
    pub fn sticky_owner(&self, model: &str) -> Option<NodeId> {
        let snap = self.snapshot();
        let holders: Vec<&NodeSnapshot> = snap
            .iter()
            .filter(|n| n.healthy && n.has_model(model))
            .collect();
        if holders.is_empty() {
            return None;
        }
        let chosen = holders
            .iter()
            .copied()
            .find(|n| n.has_model_loaded(model))
            .unwrap_or(holders[0]);
        Some(chosen.id.clone())
    }

    /// Healthy nodes' on-disk models, grouped for `/api/tags`.
    pub fn aggregated_tags(&self) -> Vec<(String, Vec<String>)> {
        let mut by_model: HashMap<String, Vec<String>> = HashMap::new();
        for node in self.snapshot() {
            if !node.healthy {
                continue;
            }
            for model in &node.models {
                by_model
                    .entry(model.clone())
                    .or_default()
                    .push(node.id.as_str().to_string());
            }
        }
        let mut models: Vec<(String, Vec<String>)> = by_model
            .into_iter()
            .map(|(model, mut nodes)| {
                nodes.sort();
                (model, nodes)
            })
            .collect();
        models.sort_by(|a, b| a.0.cmp(&b.0));
        models
    }
}

fn reconcile_ps_reservations(node: &mut NodeState) {
    let stale: Vec<u64> = node
        .reservations
        .iter()
        .filter(|(_, (model, _))| models_match(&node.loaded_models, model))
        .map(|(id, _)| *id)
        .collect();
    for id in stale {
        if let Some((_, estimate)) = node.reservations.remove(&id) {
            node.reserved_vram_gb = (node.reserved_vram_gb - estimate).max(0.0);
        }
    }
    let stale_ram: Vec<u64> = node
        .ram_reservations
        .iter()
        .filter(|(_, (model, _))| models_match(&node.loaded_models, model))
        .map(|(id, _)| *id)
        .collect();
    for id in stale_ram {
        if let Some((_, estimate)) = node.ram_reservations.remove(&id) {
            node.reserved_ram_gb = (node.reserved_ram_gb - estimate).max(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Capacity, NodeConfig, RouterConfig};

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("node id")
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

    #[test]
    fn suggested_max_inflight_tiers() {
        assert_eq!(suggested_max_inflight(0.0), 2);
        assert_eq!(suggested_max_inflight(11.9), 2);
        assert_eq!(suggested_max_inflight(12.0), 3);
        assert_eq!(suggested_max_inflight(24.0), 4);
        assert_eq!(suggested_max_inflight(48.0), 8);
    }

    #[test]
    fn inflight_inc_sets_last_client_request_at() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        assert!(registry.last_client_request_at(&id).is_none());
        registry.inflight_inc(&id);
        assert!(registry.last_client_request_at(&id).is_some());
        assert_eq!(registry.inflight(&id), 1);
        registry.inflight_dec(&id);
        assert_eq!(registry.inflight(&id), 0);
        assert!(registry.last_client_request_at(&id).is_some());
    }

    #[test]
    fn set_healthy_does_not_touch_last_client_request_at() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        registry.set_healthy(&id);
        assert!(registry.last_client_request_at(&id).is_none());
    }
}
