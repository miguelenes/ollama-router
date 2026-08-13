//! Live in-memory fleet view: health, inflight, models, reservations.

use std::collections::{HashMap, HashSet};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use crate::capacity::{
    merge_capacity, CapacityInventory, CapacityReport, CapacitySource, GpuBackend, GpuDetail,
};
use crate::config::{Capacity, HealthConfig, NodeConfig, PolicyConfig, RouterConfig};
use crate::fleet::ids::NodeId;

/// Where a live registry row came from. Reload never drops `Verda` rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeOrigin {
    Permanent,
    Verda,
}

impl NodeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Verda => "verda",
        }
    }
}

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
    /// Python-stable token (`unknown` / `ok` / `elevated` / `critical`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ok => "ok",
            Self::Elevated => "elevated",
            Self::Critical => "critical",
        }
    }

    /// Parse a capacity-agent `pressure_level` string. Unknown tokens stay `None`.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "unknown" => Some(Self::Unknown),
            "ok" => Some(Self::Ok),
            "elevated" => Some(Self::Elevated),
            "critical" => Some(Self::Critical),
            _ => None,
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
    pub vram_free_gb: Option<f64>,
    pub vram_free_known: bool,
    pub vram_used_gb: Option<f64>,
    pub vram_used_known: bool,
    pub gpu_util_pct: Option<f64>,
    pub gpu_util_known: bool,
    pub gpu_backend: GpuBackend,
    pub cpu_usage_pct: Option<f64>,
    pub ollama_running: Option<bool>,
    pub loaded_model_count: Option<u32>,
    pub disk_total_gb: Option<f64>,
    pub disk_available_gb: Option<f64>,
    pub gpus_detail: Vec<GpuSnapshot>,
    pub pressure_level: PressureLevel,
    pub fail_streak: u32,
    pub draining: bool,
    pub origin: NodeOrigin,
    pub capacity_url: Option<String>,
    pub capacity_source: CapacitySource,
    pub capacity_error: Option<String>,
    pub unhealthy_reason: Option<String>,
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

    /// Loaded-model count for metrics (agent count, else `/api/ps` set length).
    pub fn loaded_model_gauge(&self) -> u32 {
        self.loaded_model_count
            .unwrap_or(self.loaded_models.len() as u32)
    }
}

/// Bounded per-GPU row for Prometheus `{node,gpu}` series. No marketing names.
#[derive(Clone, Debug, Default)]
pub struct GpuSnapshot {
    pub index: i32,
    pub vram_total_gb: f64,
    pub vram_used_gb: f64,
    pub vram_free_gb: f64,
    pub vram_free_known: bool,
    pub vram_used_known: bool,
    pub utilization_gpu_pct: Option<f64>,
    pub temperature_c: Option<f64>,
}

const MAX_GPU_METRIC_ROWS: usize = 8;

impl GpuSnapshot {
    fn from_detail(detail: &GpuDetail) -> Self {
        Self {
            index: detail.index,
            vram_total_gb: detail.vram_total_gb,
            vram_used_gb: detail.vram_used_gb,
            vram_free_gb: detail.vram_free_gb,
            vram_free_known: detail.vram_free_is_known(),
            vram_used_known: detail.vram_used_is_known(),
            utilization_gpu_pct: detail.utilization_gpu_pct,
            temperature_c: detail.temperature_c,
        }
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
    last_client_request_at: Option<Instant>,
    registered_at: Instant,
    loaded_vram_gb: Option<f64>,
    reserved_vram_gb: f64,
    reserved_ram_gb: f64,
    reservations: HashMap<u64, (String, f64)>,
    ram_reservations: HashMap<u64, (String, f64)>,
    next_reservation_id: u64,
    ram_available_gb: Option<f64>,
    ram_available_ratio: Option<f64>,
    vram_free_gb: Option<f64>,
    vram_free_known: bool,
    vram_used_gb: Option<f64>,
    vram_used_known: bool,
    gpu_util_pct: Option<f64>,
    gpu_util_known: bool,
    gpu_backend: GpuBackend,
    cpu_usage_pct: Option<f64>,
    ollama_running: Option<bool>,
    loaded_model_count: Option<u32>,
    disk_total_gb: Option<f64>,
    disk_available_gb: Option<f64>,
    gpus_detail: Vec<GpuSnapshot>,
    pressure_level: PressureLevel,
    capacity_effective: Capacity,
    capacity_discovered: Option<Capacity>,
    capacity_source: CapacitySource,
    capacity_error: Option<String>,
    unhealthy_reason: Option<String>,
    next_probe_at: Instant,
    probe_backoff: f64,
    origin: NodeOrigin,
    draining: bool,
}

impl NodeState {
    fn from_config(config: NodeConfig, origin: NodeOrigin) -> Self {
        let merged = merge_capacity(&config.static_capacity, None, None);
        Self {
            config,
            healthy: false,
            fail_streak: 0,
            success_streak: 0,
            models: HashSet::new(),
            loaded_models: HashSet::new(),
            inflight: 0,
            last_client_request_at: None,
            registered_at: Instant::now(),
            loaded_vram_gb: None,
            reserved_vram_gb: 0.0,
            reserved_ram_gb: 0.0,
            reservations: HashMap::new(),
            ram_reservations: HashMap::new(),
            next_reservation_id: 0,
            ram_available_gb: None,
            ram_available_ratio: None,
            vram_free_gb: None,
            vram_free_known: false,
            vram_used_gb: None,
            vram_used_known: false,
            gpu_util_pct: None,
            gpu_util_known: false,
            gpu_backend: GpuBackend::Unknown,
            cpu_usage_pct: None,
            ollama_running: None,
            loaded_model_count: None,
            disk_total_gb: None,
            disk_available_gb: None,
            gpus_detail: Vec::new(),
            pressure_level: PressureLevel::Unknown,
            capacity_effective: merged.capacity,
            capacity_discovered: None,
            capacity_source: merged.source,
            capacity_error: None,
            unhealthy_reason: None,
            next_probe_at: Instant::now(),
            probe_backoff: 0.0,
            origin,
            draining: false,
        }
    }

    fn apply_permanent_config(&mut self, config: NodeConfig) {
        self.config.url = config.url;
        self.config.labels = config.labels;
        self.config.capacity_url = config.capacity_url;
        self.config.max_inflight = config.max_inflight;
        self.config.ssh = config.ssh;
        self.config.provision = config.provision;
        self.config.static_capacity = config.static_capacity;
        remarge(self);
        self.draining = false;
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
            vram_free_gb: self.vram_free_gb,
            vram_free_known: self.vram_free_known,
            vram_used_gb: self.vram_used_gb,
            vram_used_known: self.vram_used_known,
            gpu_util_pct: self.gpu_util_pct,
            gpu_util_known: self.gpu_util_known,
            gpu_backend: self.gpu_backend,
            cpu_usage_pct: self.cpu_usage_pct,
            ollama_running: self.ollama_running,
            loaded_model_count: self.loaded_model_count,
            disk_total_gb: self.disk_total_gb,
            disk_available_gb: self.disk_available_gb,
            gpus_detail: self.gpus_detail.clone(),
            pressure_level: self.pressure_level,
            fail_streak: self.fail_streak,
            draining: self.draining,
            origin: self.origin,
            capacity_url: self.config.capacity_url.clone(),
            capacity_source: self.capacity_source,
            capacity_error: self.capacity_error.clone(),
            unhealthy_reason: self.unhealthy_reason.clone(),
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
            .map(|n| {
                (
                    n.id.clone(),
                    NodeState::from_config(n.clone(), NodeOrigin::Permanent),
                )
            })
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

    /// Clone the live `NodeConfig` (SSH host may have been switched to Tailscale).
    pub fn node_config(&self, id: &NodeId) -> Option<NodeConfig> {
        self.read().nodes.get(id).map(|n| n.config.clone())
    }

    /// Set the Ollama routing URL after Tailscale verify. Refuses public IPv4.
    ///
    /// Does not count as inflight. Marks the node unhealthy so health re-probes.
    pub fn set_node_url(&self, id: &NodeId, url: &str) -> Result<(), String> {
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("empty url".into());
        }
        if crate::fleet::tailscale::url_host_is_public_ipv4(trimmed) {
            return Err("refusing public IPv4 routing URL".into());
        }
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return Ok(());
        };
        node.config.url = Some(trimmed.to_string());
        node.healthy = false;
        node.success_streak = 0;
        node.next_probe_at = Instant::now();
        Ok(())
    }

    /// Replace live labels (admin PUT). Does not write fleet.yaml.
    pub fn set_node_labels(&self, id: &NodeId, labels: Vec<String>) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.config.labels = labels;
        }
    }

    /// Point ordinary OpenSSH at a Tailscale IPv4 (same key/user).
    pub fn set_ssh_endpoint(&self, id: &NodeId, host: &str, port: u16) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            if let Some(ssh) = node.config.ssh.as_mut() {
                ssh.host = host.to_string();
                ssh.port = port;
            }
        }
    }

    /// Clear a non-Tailscale IP routing URL after a failed provision.
    pub fn clear_unsafe_routing_url(&self, id: &NodeId) {
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        let Some(url) = node.config.url.as_deref() else {
            return;
        };
        if crate::fleet::tailscale::url_host_is_tailscale(url) {
            return;
        }
        if crate::fleet::tailscale::url_host_is_ip(url) {
            node.config.url = None;
        }
    }

    /// Instant the node was first registered (idle grace).
    pub fn registered_at(&self, id: &NodeId) -> Option<Instant> {
        self.read().nodes.get(id).map(|n| n.registered_at)
    }

    /// Mark a node healthy (tests). Bypasses `success_threshold`.
    pub fn set_healthy(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.healthy = true;
            node.fail_streak = 0;
            node.success_streak = self.health.success_threshold;
            node.unhealthy_reason = None;
            node.probe_backoff = 0.0;
        }
    }

    /// Mark a node unhealthy (tests).
    pub fn set_unhealthy(&self, id: &NodeId) {
        self.mark_unhealthy(id, None);
    }

    /// Mark unhealthy with an allowlisted reason (`no_url`, `public_url_blocked`, …).
    pub fn mark_unhealthy(&self, id: &NodeId, reason: Option<&str>) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.healthy = false;
            node.success_streak = 0;
            node.unhealthy_reason = reason.map(str::to_string);
        }
    }

    /// Immediate bench for missing / public URLs (fail streak at threshold).
    pub fn mark_unreachable(&self, id: &NodeId, reason: &str) {
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        node.healthy = false;
        node.success_streak = 0;
        node.fail_streak = node.fail_streak.max(self.health.fail_streak_threshold);
        node.unhealthy_reason = Some(reason.to_string());
        node.probe_backoff = self.health.interval_seconds;
        node.next_probe_at = Instant::now()
            + std::time::Duration::from_secs_f64(self.health.interval_seconds.max(0.05));
    }

    /// Record a successful `/api/tags` probe. `success_threshold` gates recovery.
    pub fn note_probe_success(&self, id: &NodeId) {
        let threshold = self.health.success_threshold;
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.success_streak = node.success_streak.saturating_add(1);
            node.fail_streak = 0;
            if !node.healthy && node.success_streak >= threshold {
                node.healthy = true;
                node.unhealthy_reason = None;
                node.probe_backoff = 0.0;
            }
        }
    }

    /// Record a failed `/api/tags` probe. Unhealthy once fail streak hits threshold.
    pub fn note_probe_failure(&self, id: &NodeId, reason: &str) {
        let threshold = self.health.fail_streak_threshold;
        let interval = self.health.interval_seconds;
        let backoff_max = self.health.backoff_max_seconds;
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.fail_streak = node.fail_streak.saturating_add(1);
            node.success_streak = 0;
            if node.healthy && node.fail_streak >= threshold {
                node.healthy = false;
                node.unhealthy_reason = Some(reason.to_string());
            }
            if !node.healthy {
                let doubled = if node.probe_backoff > 0.0 {
                    node.probe_backoff * 2.0
                } else {
                    interval
                };
                node.probe_backoff = doubled.max(interval).min(backoff_max);
            } else {
                node.probe_backoff = 0.0;
            }
        }
    }

    /// Schedule the next probe instant (health loop + tests).
    pub fn set_next_probe_at(&self, id: &NodeId, at: Instant) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.next_probe_at = at;
        }
    }

    /// When the next probe is due.
    pub fn next_probe_at(&self, id: &NodeId) -> Option<Instant> {
        self.read().nodes.get(id).map(|n| n.next_probe_at)
    }

    /// Current backoff delay in seconds (0 while healthy).
    pub fn probe_backoff(&self, id: &NodeId) -> f64 {
        self.read()
            .nodes
            .get(id)
            .map(|n| n.probe_backoff)
            .unwrap_or(0.0)
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
        remarge(node);
    }

    /// Admit a client generate/chat/embed forward. Writes `last_client_request_at`.
    pub fn inflight_inc(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_add(1);
            node.last_client_request_at = Some(Instant::now());
        }
    }

    /// Release one inflight slot. Draining permanent nodes with zero inflight are dropped.
    pub fn inflight_dec(&self, id: &NodeId) {
        let mut inner = self.write();
        if let Some(node) = inner.nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_sub(1);
        }
        sweep_drained(&mut inner);
    }

    /// Occupy an inflight slot without touching idle (warm-keeper only).
    pub fn occupancy_inc(&self, id: &NodeId) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_add(1);
        }
    }

    /// Release a warm-keeper occupancy slot. Sweeps drained nodes.
    pub fn occupancy_dec(&self, id: &NodeId) {
        let mut inner = self.write();
        if let Some(node) = inner.nodes.get_mut(id) {
            node.inflight = node.inflight.saturating_sub(1);
        }
        sweep_drained(&mut inner);
    }

    /// In-process idle timestamp of last client generate/chat/embed.
    pub fn last_client_request_at(&self, id: &NodeId) -> Option<Instant> {
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
            node.unhealthy_reason = Some("probe_failures".into());
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

    /// Store agent-reported pressure (soft-fail capacity probe). Does not change health.
    pub fn set_pressure_level(&self, id: &NodeId, level: PressureLevel) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.pressure_level = level;
        }
    }

    /// Merge a successful capacity-agent report. Never flips health.
    pub fn apply_capacity_report(
        &self,
        id: &NodeId,
        report: &CapacityReport,
        pressure_level: Option<PressureLevel>,
    ) {
        let mut inner = self.write();
        let Some(node) = inner.nodes.get_mut(id) else {
            return;
        };
        node.capacity_discovered = Some(report.as_capacity());
        node.vram_free_gb = Some(report.vram_free_gb);
        node.vram_free_known = report.vram_free_is_known();
        node.vram_used_gb = Some(report.vram_used_gb);
        node.vram_used_known = report.vram_used_is_known();
        node.gpu_backend = report.gpu_backend.unwrap_or_default();
        let utils: Vec<f64> = report
            .gpus_detail
            .iter()
            .filter_map(|gpu| gpu.utilization_gpu_pct)
            .collect();
        if utils.is_empty() {
            node.gpu_util_pct = None;
            node.gpu_util_known = false;
        } else {
            node.gpu_util_pct = Some(utils.iter().sum::<f64>() / utils.len() as f64);
            node.gpu_util_known = true;
        }
        node.cpu_usage_pct = report.cpu_usage_pct.or_else(|| {
            report
                .pressure
                .as_ref()
                .and_then(|pressure| pressure.cpu_usage_pct)
        });
        node.ollama_running = report.ollama_running;
        node.loaded_model_count = report.loaded_model_count.map(|n| n.max(0) as u32);
        node.disk_total_gb = report.disk_total_gb;
        node.disk_available_gb = report.disk_available_gb;
        node.gpus_detail = report
            .gpus_detail
            .iter()
            .take(MAX_GPU_METRIC_ROWS)
            .map(GpuSnapshot::from_detail)
            .collect();
        if let Some(pressure) = report.pressure.as_ref() {
            node.ram_available_gb = pressure.ram_available_gb;
            node.ram_available_ratio = pressure.ram_available_ratio;
        }
        if let Some(level) = pressure_level {
            node.pressure_level = level;
        }
        node.capacity_error = None;
        remarge(node);
    }

    /// Allowlisted capacity miss (`http_status` / `timeout` / `unreachable` / `parse`).
    pub fn set_capacity_error(&self, id: &NodeId, reason: &str) {
        if let Some(node) = self.write().nodes.get_mut(id) {
            node.capacity_error = Some(reason.to_string());
        }
    }

    /// Insert or refresh a fleet.yaml node without resetting inflight / idle / reservations.
    /// Existing Verda rows with the same id are left untouched.
    pub fn upsert_permanent(&self, config: NodeConfig) {
        let mut inner = self.write();
        upsert_permanent_locked(&mut inner, config);
    }

    /// Exclude a permanent node from ranking immediately; drop it when inflight hits 0.
    pub fn begin_remove_permanent(&self, id: &NodeId) {
        let mut inner = self.write();
        begin_remove_permanent_locked(&mut inner, id);
        sweep_drained(&mut inner);
    }

    /// Diff fleet.yaml membership by `NodeId`. Never deletes Verda rows.
    pub fn apply_permanent_inventory(&self, nodes: &[NodeConfig]) {
        let mut inner = self.write();
        let incoming: HashSet<NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
        let stale: Vec<NodeId> = inner
            .nodes
            .iter()
            .filter(|(_, n)| n.origin == NodeOrigin::Permanent && !incoming.contains(&n.config.id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            begin_remove_permanent_locked(&mut inner, &id);
        }
        for node in nodes {
            upsert_permanent_locked(&mut inner, node.clone());
        }
        sweep_drained(&mut inner);
    }

    /// Insert or refresh a Verda ephemeral (tests / future manager). Does not overwrite Permanent.
    pub fn upsert_verda(&self, config: NodeConfig) {
        let mut inner = self.write();
        let id = config.id.clone();
        match inner.nodes.get_mut(&id) {
            Some(existing) if existing.origin == NodeOrigin::Verda => {
                existing.apply_permanent_config(config);
            }
            Some(_) => {}
            None => {
                inner
                    .nodes
                    .insert(id, NodeState::from_config(config, NodeOrigin::Verda));
            }
        }
    }

    /// Drop a Verda node from the live registry. Permanent hosts are ignored.
    pub fn remove_verda(&self, id: &NodeId) {
        let mut inner = self.write();
        if inner
            .nodes
            .get(id)
            .is_some_and(|n| n.origin == NodeOrigin::Verda)
        {
            inner.nodes.remove(id);
        }
    }

    /// Sticky-affinity owner: prefer a warm healthy holder of `model`.
    pub fn sticky_owner(&self, model: &str) -> Option<NodeId> {
        let snap = self.snapshot();
        let holders: Vec<&NodeSnapshot> = snap
            .iter()
            .filter(|n| n.healthy && !n.draining && n.has_model(model))
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
            if !node.healthy || node.draining {
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

fn remarge(node: &mut NodeState) {
    let merged = merge_capacity(
        &node.config.static_capacity,
        node.capacity_discovered.as_ref(),
        node.loaded_vram_gb,
    );
    node.capacity_effective = merged.capacity;
    node.capacity_source = merged.source;
}

fn upsert_permanent_locked(inner: &mut Inner, config: NodeConfig) {
    let id = config.id.clone();
    match inner.nodes.get_mut(&id) {
        Some(existing) if existing.origin == NodeOrigin::Verda => {
            tracing::warn!(
                node = %id,
                "fleet.yaml id collides with a Verda node; leaving Verda row in place"
            );
        }
        Some(existing) => {
            existing.origin = NodeOrigin::Permanent;
            existing.apply_permanent_config(config);
        }
        None => {
            inner
                .nodes
                .insert(id, NodeState::from_config(config, NodeOrigin::Permanent));
        }
    }
}

fn begin_remove_permanent_locked(inner: &mut Inner, id: &NodeId) {
    let Some(node) = inner.nodes.get_mut(id) else {
        return;
    };
    if node.origin != NodeOrigin::Permanent {
        return;
    }
    node.draining = true;
}

fn sweep_drained(inner: &mut Inner) {
    inner.nodes.retain(|_, node| {
        !(node.origin == NodeOrigin::Permanent && node.draining && node.inflight == 0)
    });
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
    use std::collections::HashSet;

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
    fn apply_permanent_inventory_adds_and_updates_without_resetting_inflight() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        registry.set_healthy(&id);
        registry.inflight_inc(&id);
        let last = registry.last_client_request_at(&id);

        let mut updated = node("a", 16.0, 1);
        updated.url = Some("http://a-new:11434".into());
        updated.labels = vec!["gpu".into()];
        registry.apply_permanent_inventory(&[updated, node("b", 8.0, 1)]);

        let snap_a = registry.get(&id).unwrap();
        assert_eq!(snap_a.url.as_deref(), Some("http://a-new:11434"));
        assert_eq!(snap_a.labels, ["gpu"]);
        assert_eq!(snap_a.capacity.vram_gb, Some(16.0));
        assert_eq!(snap_a.inflight, 1);
        assert_eq!(snap_a.origin, NodeOrigin::Permanent);
        assert!(!snap_a.draining);
        assert_eq!(registry.last_client_request_at(&id), last);
        assert!(registry.get(&nid("b")).is_some());
    }

    #[test]
    fn remove_permanent_with_inflight_drains_then_drops() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1), node("b", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let a = nid("a");
        registry.set_healthy(&a);
        registry.set_healthy(&nid("b"));
        registry.inflight_inc(&a);

        registry.apply_permanent_inventory(&[node("b", 8.0, 1)]);
        let snap = registry.get(&a).unwrap();
        assert!(snap.draining);
        assert_eq!(snap.inflight, 1);

        let ranked = crate::routing::rank_nodes(
            &registry.snapshot(),
            crate::routing::RequestClass::Small,
            None,
            registry.policy(),
            None,
            &HashSet::new(),
            0,
        );
        assert!(ranked.ranked.iter().all(|n| n.id.as_str() != "a"));

        registry.inflight_dec(&a);
        assert!(registry.get(&a).is_none());
        assert!(registry.get(&nid("b")).is_some());
    }

    #[test]
    fn apply_permanent_inventory_does_not_drop_verda() {
        let config = RouterConfig {
            nodes: vec![node("desk", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.upsert_verda(node("spot-1", 24.0, 1));
        registry.apply_permanent_inventory(&[]);
        assert!(registry.get(&nid("desk")).is_none());
        let spot = registry.get(&nid("spot-1")).unwrap();
        assert_eq!(spot.origin, NodeOrigin::Verda);
        assert!(!spot.draining);
    }

    #[test]
    fn fleet_yaml_id_does_not_overwrite_verda() {
        let registry = Registry::new(&RouterConfig::default());
        registry.upsert_verda(node("shared", 24.0, 1));
        let mut permanent = node("shared", 8.0, 1);
        permanent.url = Some("http://fleet:11434".into());
        registry.upsert_permanent(permanent);
        let snap = registry.get(&nid("shared")).unwrap();
        assert_eq!(snap.origin, NodeOrigin::Verda);
        assert_eq!(snap.url.as_deref(), Some("http://shared:11434"));
    }

    #[test]
    fn occupancy_does_not_set_last_client_request_at() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        registry.occupancy_inc(&id);
        assert_eq!(registry.inflight(&id), 1);
        assert!(registry.last_client_request_at(&id).is_none());
        registry.occupancy_dec(&id);
        assert_eq!(registry.inflight(&id), 0);
    }

    #[test]
    fn note_probe_success_respects_success_threshold() {
        let mut config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        config.health.success_threshold = 2;
        let registry = Registry::new(&config);
        let id = nid("a");
        registry.note_probe_success(&id);
        assert!(!registry.get(&id).unwrap().healthy);
        registry.note_probe_success(&id);
        assert!(registry.get(&id).unwrap().healthy);
    }

    #[test]
    fn mark_unreachable_sets_reason() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        registry.set_healthy(&id);
        registry.mark_unreachable(&id, "public_url_blocked");
        let snap = registry.get(&id).unwrap();
        assert!(!snap.healthy);
        assert_eq!(snap.unhealthy_reason.as_deref(), Some("public_url_blocked"));
        assert!(snap.fail_streak >= 3);
    }

    #[test]
    fn set_node_url_refuses_public_ipv4() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        let err = registry
            .set_node_url(&id, "http://8.8.8.8:11434")
            .expect_err("public");
        assert!(err.contains("public IPv4"));
        assert_eq!(
            registry.get(&id).unwrap().url.as_deref(),
            Some("http://a:11434")
        );
        registry
            .set_node_url(&id, "http://100.64.0.9:11434")
            .expect("tailscale");
        assert_eq!(
            registry.get(&id).unwrap().url.as_deref(),
            Some("http://100.64.0.9:11434")
        );
        assert!(!registry.get(&id).unwrap().healthy);
    }

    #[test]
    fn remove_verda_spares_permanent() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.remove_verda(&nid("a"));
        assert!(registry.get(&nid("a")).is_some());
        registry.upsert_verda(node("spot", 24.0, 1));
        registry.remove_verda(&nid("spot"));
        assert!(registry.get(&nid("spot")).is_none());
    }

    #[test]
    fn apply_capacity_report_stores_util_and_known_flags() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        let mut report = CapacityReport {
            vram_gb: 8.0,
            gpus: 1,
            ram_gb: 32.0,
            vram_used_gb: 8.0,
            vram_free_gb: 0.0,
            vram_free_known: Some(true),
            vram_used_known: Some(true),
            gpu_backend: Some(GpuBackend::Cuda),
            cpu_usage_pct: Some(12.0),
            ollama_running: Some(true),
            loaded_model_count: Some(2),
            ..CapacityReport::default()
        };
        report.gpus_detail.push(GpuDetail {
            index: 0,
            vram_total_gb: 8.0,
            vram_used_gb: 8.0,
            vram_free_gb: 0.0,
            utilization_gpu_pct: Some(91.0),
            vram_free_known: Some(true),
            vram_used_known: Some(true),
            util_known: Some(true),
            ..GpuDetail::default()
        });
        report.pressure = Some(crate::capacity::Pressure {
            ram_available_gb: Some(24.0),
            ram_available_ratio: Some(0.75),
            ..Default::default()
        });
        registry.apply_capacity_report(&id, &report, Some(PressureLevel::Ok));
        let snap = registry.get(&id).unwrap();
        assert!(snap.vram_free_known);
        assert!((snap.vram_free_gb.unwrap() - 0.0).abs() < 1e-12);
        assert!(snap.gpu_util_known);
        assert!((snap.gpu_util_pct.unwrap() - 91.0).abs() < 1e-9);
        assert_eq!(snap.gpu_backend, GpuBackend::Cuda);
        assert_eq!(snap.cpu_usage_pct, Some(12.0));
        assert_eq!(snap.loaded_model_gauge(), 2);
        assert_eq!(snap.ram_available_gb, Some(24.0));
        assert_eq!(snap.gpus_detail.len(), 1);
    }

    #[test]
    fn apply_capacity_report_unknown_free_when_cpu_inventory() {
        let config = RouterConfig {
            nodes: vec![node("cpu", 0.0, 0)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("cpu");
        let report = CapacityReport {
            vram_gb: 0.0,
            gpus: 0,
            ram_gb: 16.0,
            vram_free_gb: 0.0,
            vram_free_known: Some(false),
            gpu_backend: Some(GpuBackend::Cpu),
            ..CapacityReport::default()
        };
        registry.apply_capacity_report(&id, &report, None);
        let snap = registry.get(&id).unwrap();
        assert!(!snap.vram_free_known);
        assert!(!snap.gpu_util_known);
        assert_eq!(snap.gpu_backend, GpuBackend::Cpu);
    }
}
