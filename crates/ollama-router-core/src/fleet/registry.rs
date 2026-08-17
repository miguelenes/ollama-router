//! Live in-memory fleet view: health, inflight, models, reservations.
//!
//! Membership (`HashMap` of `Arc<Node>`) is guarded by a process-wide `RwLock`
//! taken only on insert/remove/upsert. Per-request counters live in atomics on
//! each `Node` so generate/chat/embed do not take a global write lock.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use crate::capacity::{
    merge_capacity, CapacityInventory, CapacityReport, CapacitySource, GpuBackend, GpuDetail,
};
use crate::config::{Capacity, HealthConfig, NodeConfig, PolicyConfig, RouterConfig};
use crate::fleet::ids::NodeId;
use crate::fleet::tags::{
    merge_catalog, merge_ps, AggregatedPs, AggregatedTag, CatalogNode, PsNode, PsRecord, TagRecord,
};

/// Where a live registry row came from. Reload never drops cloud rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeOrigin {
    Permanent,
    /// Debug/adopt admin surface (`PUT /router/v1/nodes`, enroll `origin: adopt`).
    Adopt,
    Verda,
    Runpod,
}

impl NodeOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Adopt => "adopt",
            Self::Verda => "verda",
            Self::Runpod => "runpod",
        }
    }
}

/// Outcome of [`Registry::inflight_inc`]. Distinguishes drain vs cap refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InflightAdmit {
    /// Slot claimed; `last_client_request_at` written.
    Admitted,
    /// Node is not in the live map.
    Missing,
    /// Verda draining or forget-pending; caller should retry another node.
    Draining,
    /// Effective inflight cap reached; caller should retry another node.
    Saturated,
}

impl InflightAdmit {
    /// True when a slot was claimed.
    pub fn admitted(self) -> bool {
        matches!(self, Self::Admitted)
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

    /// Parse a node-agent `pressure_level` string. Unknown tokens stay `None`.
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

fn effective_max_inflight(
    explicit: Option<u32>,
    default: Option<u32>,
    vram_gb: f64,
    pressure: PressureLevel,
) -> u32 {
    if let Some(max) = explicit {
        return max;
    }
    if let Some(max) = default {
        return max;
    }
    let mut effective = suggested_max_inflight(vram_gb);
    match pressure {
        PressureLevel::Elevated => effective = effective.saturating_sub(1).max(1),
        PressureLevel::Critical => effective = 1,
        PressureLevel::Unknown | PressureLevel::Ok => {}
    }
    effective
}

fn finite_or_none(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn finite_opt(value: Option<f64>) -> Option<f64> {
    value.and_then(finite_or_none)
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

static PROCESS_EPOCH: OnceLock<Instant> = OnceLock::new();

fn process_epoch() -> Instant {
    *PROCESS_EPOCH.get_or_init(Instant::now)
}

/// Monotonic millis since process epoch. `0` is reserved for "never".
fn now_ms() -> u64 {
    let ms = process_epoch()
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX));
    (ms as u64).max(1)
}

fn instant_from_ms(ms: u64) -> Instant {
    process_epoch() + Duration::from_millis(ms)
}

fn load_f64(atom: &AtomicU64) -> f64 {
    f64::from_bits(atom.load(Ordering::Relaxed))
}

fn store_f64(atom: &AtomicU64, value: f64) {
    atom.store(value.to_bits(), Ordering::Relaxed);
}

fn saturating_fetch_add(atom: &AtomicU32) {
    let _ = atom.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_add(1))
    });
}

/// Saturating decrement; returns the new value.
fn saturating_fetch_sub(atom: &AtomicU32) -> u32 {
    atom.fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
        Some(v.saturating_sub(1))
    })
    .map_or(0, |prev| prev.saturating_sub(1))
}

fn opt_arc_str(value: Option<String>) -> Option<Arc<str>> {
    value.map(Arc::from)
}

/// Ranking / forwarding snapshot of one node. Pure; no locks.
#[derive(Clone, Debug)]
pub struct NodeSnapshot {
    pub id: NodeId,
    pub url: Option<Arc<str>>,
    pub labels: Arc<[String]>,
    pub healthy: bool,
    pub models: Arc<HashSet<String>>,
    pub loaded_models: Arc<HashSet<String>>,
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
    pub gpus_detail: Arc<[GpuSnapshot]>,
    pub pressure_level: PressureLevel,
    pub fail_streak: u32,
    pub draining: bool,
    /// Operator maintenance cordon (admin drain). Not inventory/Verda `draining`.
    pub cordoned: bool,
    pub origin: NodeOrigin,
    pub capacity_url: Option<Arc<str>>,
    pub capacity_source: CapacitySource,
    pub capacity_error: Option<Arc<str>>,
    pub unhealthy_reason: Option<Arc<str>>,
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

    /// Effective VRAM (0 when unset). Prefer [`Self::known_vram_gb`] for ranking gates.
    pub fn vram_gb(&self) -> f64 {
        self.capacity.vram_gb()
    }

    /// Measured/declared VRAM, or `None` when omitted (unknown — not a CPU).
    pub fn known_vram_gb(&self) -> Option<f64> {
        self.capacity.known_vram_gb()
    }

    /// Effective RAM (0 when unset).
    pub fn ram_gb(&self) -> f64 {
        self.capacity.ram_gb()
    }

    /// Effective GPU count (0 when unset). Prefer [`Self::known_gpus`] for ranking.
    pub fn gpus(&self) -> u32 {
        self.capacity.gpus()
    }

    /// Measured/declared GPU count, or `None` when omitted (unknown).
    pub fn known_gpus(&self) -> Option<u32> {
        self.capacity.known_gpus()
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
        effective_max_inflight(
            self.max_inflight,
            default_max_inflight,
            self.vram_gb(),
            self.pressure_level,
        )
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
#[derive(Clone, Copy, Debug, Default)]
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
            vram_total_gb: finite_or_none(detail.vram_total_gb).unwrap_or(0.0),
            vram_used_gb: finite_or_none(detail.vram_used_gb).unwrap_or(0.0),
            vram_free_gb: finite_or_none(detail.vram_free_gb).unwrap_or(0.0),
            vram_free_known: detail.vram_free_is_known() && detail.vram_free_gb.is_finite(),
            vram_used_known: detail.vram_used_is_known() && detail.vram_used_gb.is_finite(),
            utilization_gpu_pct: finite_opt(detail.utilization_gpu_pct),
            temperature_c: finite_opt(detail.temperature_c),
        }
    }
}

#[derive(Default)]
struct ReservationLedger {
    vram: HashMap<u64, (String, f64)>,
    ram: HashMap<u64, (String, f64)>,
    next_id: u64,
    reserved_vram_gb: f64,
    reserved_ram_gb: f64,
}

struct Cold {
    url: Option<Arc<str>>,
    capacity_url: Option<Arc<str>>,
    labels: Arc<[String]>,
    max_inflight: Option<u32>,
    static_capacity: Capacity,
    healthy: bool,
    success_streak: u32,
    models: Arc<HashSet<String>>,
    tag_records: Arc<HashMap<String, TagRecord>>,
    loaded_models: Arc<HashSet<String>>,
    ps_records: Arc<HashMap<String, PsRecord>>,
    loaded_vram_gb: Option<f64>,
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
    gpus_detail: Arc<[GpuSnapshot]>,
    pressure_level: PressureLevel,
    capacity_effective: Capacity,
    capacity_discovered: Option<Capacity>,
    capacity_source: CapacitySource,
    capacity_error: Option<Arc<str>>,
    unhealthy_reason: Option<Arc<str>>,
    next_probe_at: Instant,
    probe_backoff: f64,
}

struct Node {
    id: NodeId,
    origin: NodeOrigin,
    registered_at: Instant,
    inflight: AtomicU32,
    last_client_ms: AtomicU64,
    reserved_vram_bits: AtomicU64,
    reserved_ram_bits: AtomicU64,
    draining: AtomicBool,
    /// Operator cordon; set only by admin drain/undrain. Survives inventory reload.
    cordoned: AtomicBool,
    forget_pending: AtomicBool,
    fail_streak: AtomicU32,
    reservations: Mutex<ReservationLedger>,
    cold: RwLock<Cold>,
}

impl Node {
    fn from_config(config: NodeConfig, origin: NodeOrigin) -> Self {
        let merged = merge_capacity(&config.static_capacity, None, None);
        let id = config.id.clone();
        Self {
            id,
            origin,
            registered_at: Instant::now(),
            inflight: AtomicU32::new(0),
            last_client_ms: AtomicU64::new(0),
            reserved_vram_bits: AtomicU64::new(0.0f64.to_bits()),
            reserved_ram_bits: AtomicU64::new(0.0f64.to_bits()),
            draining: AtomicBool::new(false),
            cordoned: AtomicBool::new(false),
            forget_pending: AtomicBool::new(false),
            fail_streak: AtomicU32::new(0),
            reservations: Mutex::new(ReservationLedger::default()),
            cold: RwLock::new(Cold {
                url: opt_arc_str(config.url),
                capacity_url: opt_arc_str(config.capacity_url),
                labels: Arc::from(config.labels),
                max_inflight: config.max_inflight,
                static_capacity: config.static_capacity,
                healthy: false,
                success_streak: 0,
                models: Arc::new(HashSet::new()),
                tag_records: Arc::new(HashMap::new()),
                loaded_models: Arc::new(HashSet::new()),
                ps_records: Arc::new(HashMap::new()),
                loaded_vram_gb: None,
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
                gpus_detail: Arc::from(Vec::new()),
                pressure_level: PressureLevel::Unknown,
                capacity_effective: merged.capacity,
                capacity_discovered: None,
                capacity_source: merged.source,
                capacity_error: None,
                unhealthy_reason: None,
                next_probe_at: Instant::now(),
                probe_backoff: 0.0,
            }),
        }
    }

    fn cold_read(&self) -> RwLockReadGuard<'_, Cold> {
        self.cold
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cold_write(&self) -> RwLockWriteGuard<'_, Cold> {
        self.cold
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ledger(&self) -> std::sync::MutexGuard<'_, ReservationLedger> {
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn apply_permanent_config(&self, config: NodeConfig) {
        let mut cold = self.cold_write();
        cold.url = opt_arc_str(config.url);
        cold.labels = Arc::from(config.labels);
        cold.capacity_url = opt_arc_str(config.capacity_url);
        cold.max_inflight = config.max_inflight;
        cold.static_capacity = config.static_capacity;
        remarge(&mut cold);
        drop(cold);
        self.draining.store(false, Ordering::Release);
        // Operator cordon is independent of inventory `draining` and must survive
        // in-process fleet.yaml reload (`apply_permanent_config`).
        self.forget_pending.store(false, Ordering::Release);
    }

    fn snapshot(&self) -> NodeSnapshot {
        let cold = self.cold_read();
        NodeSnapshot {
            id: self.id.clone(),
            url: cold.url.clone(),
            labels: Arc::clone(&cold.labels),
            healthy: cold.healthy,
            models: Arc::clone(&cold.models),
            loaded_models: Arc::clone(&cold.loaded_models),
            inflight: self.inflight.load(Ordering::Relaxed),
            max_inflight: cold.max_inflight,
            capacity: cold.capacity_effective,
            reserved_vram_gb: load_f64(&self.reserved_vram_bits),
            reserved_ram_gb: load_f64(&self.reserved_ram_bits),
            loaded_vram_gb: cold.loaded_vram_gb,
            ram_available_gb: cold.ram_available_gb,
            ram_available_ratio: cold.ram_available_ratio,
            vram_free_gb: cold.vram_free_gb,
            vram_free_known: cold.vram_free_known,
            vram_used_gb: cold.vram_used_gb,
            vram_used_known: cold.vram_used_known,
            gpu_util_pct: cold.gpu_util_pct,
            gpu_util_known: cold.gpu_util_known,
            gpu_backend: cold.gpu_backend,
            cpu_usage_pct: cold.cpu_usage_pct,
            ollama_running: cold.ollama_running,
            loaded_model_count: cold.loaded_model_count,
            disk_total_gb: cold.disk_total_gb,
            disk_available_gb: cold.disk_available_gb,
            gpus_detail: Arc::clone(&cold.gpus_detail),
            pressure_level: cold.pressure_level,
            fail_streak: self.fail_streak.load(Ordering::Relaxed),
            draining: self.draining.load(Ordering::Acquire),
            cordoned: self.cordoned.load(Ordering::Acquire),
            origin: self.origin,
            capacity_url: cold.capacity_url.clone(),
            capacity_source: cold.capacity_source,
            capacity_error: cold.capacity_error.clone(),
            unhealthy_reason: cold.unhealthy_reason.clone(),
        }
    }

    fn node_config(&self) -> NodeConfig {
        let cold = self.cold_read();
        NodeConfig {
            id: self.id.clone(),
            url: cold.url.as_ref().map(|s| s.to_string()),
            capacity_url: cold.capacity_url.as_ref().map(|s| s.to_string()),
            labels: cold.labels.to_vec(),
            static_capacity: cold.static_capacity,
            max_inflight: cold.max_inflight,
        }
    }

    fn last_client_request_at(&self) -> Option<Instant> {
        match self.last_client_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(instant_from_ms(ms)),
        }
    }

    fn is_droppable(&self) -> bool {
        if self.inflight.load(Ordering::Acquire) != 0 {
            return false;
        }
        match self.origin {
            NodeOrigin::Permanent | NodeOrigin::Adopt => self.draining.load(Ordering::Acquire),
            NodeOrigin::Verda | NodeOrigin::Runpod => self.forget_pending.load(Ordering::Acquire),
        }
    }

    fn sync_reserved(&self, ledger: &ReservationLedger) {
        store_f64(&self.reserved_vram_bits, ledger.reserved_vram_gb);
        store_f64(&self.reserved_ram_bits, ledger.reserved_ram_gb);
    }
}

struct Inner {
    nodes: HashMap<NodeId, Arc<Node>>,
}

/// Live fleet registry. Safe to share across Axum tasks (`Arc<Registry>`).
pub struct Registry {
    inner: RwLock<Inner>,
    health: HealthConfig,
    policy: PolicyConfig,
    public_share_suffixes: Vec<String>,
    /// Wakes saturation waiters when an inflight slot frees (`notify_waiters`).
    slot_notify: Arc<tokio::sync::Notify>,
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
                    Arc::new(Node::from_config(n.clone(), NodeOrigin::Permanent)),
                )
            })
            .collect();
        Self {
            inner: RwLock::new(Inner { nodes }),
            health: config.health.clone(),
            policy: config.policy.clone(),
            public_share_suffixes: config.tunnel.public_share_suffixes.clone(),
            slot_notify: Arc::new(tokio::sync::Notify::new()),
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

    fn node(&self, id: &NodeId) -> Option<Arc<Node>> {
        self.read().nodes.get(id).cloned()
    }

    /// Drop this row if it is still the same `Arc` and is drain-idle.
    ///
    /// Must not be called while holding a per-node lock.
    fn remove_if_droppable(&self, id: &NodeId, expected: &Arc<Node>) {
        if !expected.is_droppable() {
            return;
        }
        let mut inner = self.write();
        if inner
            .nodes
            .get(id)
            .is_some_and(|n| Arc::ptr_eq(n, expected) && n.is_droppable())
        {
            inner.nodes.remove(id);
        }
    }

    /// Policy snapshot used by ranking.
    pub fn policy(&self) -> &PolicyConfig {
        &self.policy
    }

    /// Health knobs (fail credits).
    pub fn health(&self) -> &HealthConfig {
        &self.health
    }

    /// Extra public-share hostname suffixes (`.zrok.io` is always blocked).
    pub fn public_share_suffixes(&self) -> &[String] {
        &self.public_share_suffixes
    }

    /// Ranking snapshot of every configured node.
    pub fn snapshot(&self) -> Vec<NodeSnapshot> {
        let nodes: Vec<Arc<Node>> = {
            let inner = self.read();
            inner.nodes.values().cloned().collect()
        };
        nodes.iter().map(|n| n.snapshot()).collect()
    }

    /// Snapshot of one node.
    pub fn get(&self, id: &NodeId) -> Option<NodeSnapshot> {
        self.node(id).map(|n| n.snapshot())
    }

    /// Clone the live `NodeConfig`.
    ///
    /// The routing URL may have been set by enroll to a zrok private share;
    /// the router never SSHes.
    pub fn node_config(&self, id: &NodeId) -> Option<NodeConfig> {
        self.node(id).map(|n| n.node_config())
    }

    /// Set the Ollama routing URL after enroll.
    ///
    /// Refuses public IPs and public-share hostnames. Does not count as inflight.
    /// Marks the node unhealthy so health re-probes.
    pub fn set_node_url(&self, id: &NodeId, url: &str) -> Result<(), String> {
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("empty url".into());
        }
        if crate::fleet::url_policy::url_host_is_public_ip(trimmed) {
            return Err("refusing public IP routing URL".into());
        }
        if crate::fleet::url_policy::url_host_is_public_share(trimmed, &self.public_share_suffixes)
        {
            return Err("refusing public share routing URL".into());
        }
        let Some(node) = self.node(id) else {
            return Ok(());
        };
        let mut cold = node.cold_write();
        cold.url = Some(Arc::from(trimmed));
        cold.healthy = false;
        cold.success_streak = 0;
        cold.next_probe_at = Instant::now();
        Ok(())
    }

    /// Set the node-agent URL (separate zrok frontend). Same public-URL rules.
    pub fn set_capacity_url(&self, id: &NodeId, url: &str) -> Result<(), String> {
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("empty capacity_url".into());
        }
        if crate::fleet::url_policy::url_host_is_public_ip(trimmed) {
            return Err("refusing public IP capacity URL".into());
        }
        if crate::fleet::url_policy::url_host_is_public_share(trimmed, &self.public_share_suffixes)
        {
            return Err("refusing public share capacity URL".into());
        }
        if let Some(node) = self.node(id) {
            node.cold_write().capacity_url = Some(Arc::from(trimmed));
        }
        Ok(())
    }

    /// Live origin for `id`, if the node exists.
    pub fn origin(&self, id: &NodeId) -> Option<NodeOrigin> {
        self.node(id).map(|n| n.origin)
    }

    /// Replace live labels (admin PUT). Does not write fleet.yaml.
    pub fn set_node_labels(&self, id: &NodeId, labels: Vec<String>) {
        if let Some(node) = self.node(id) {
            node.cold_write().labels = Arc::from(labels);
        }
    }

    /// Instant the node was first registered (idle grace).
    pub fn registered_at(&self, id: &NodeId) -> Option<Instant> {
        self.node(id).map(|n| n.registered_at)
    }

    /// Mark a node healthy (tests). Bypasses `success_threshold`.
    pub fn set_healthy(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        node.fail_streak.store(0, Ordering::Relaxed);
        let mut cold = node.cold_write();
        cold.healthy = true;
        cold.success_streak = self.health.success_threshold;
        cold.unhealthy_reason = None;
        cold.probe_backoff = 0.0;
    }

    /// Mark a node unhealthy (tests).
    pub fn set_unhealthy(&self, id: &NodeId) {
        self.mark_unhealthy(id, None);
    }

    /// Mark unhealthy with an allowlisted reason (`no_url`, `public_url_blocked`, …).
    pub fn mark_unhealthy(&self, id: &NodeId, reason: Option<&str>) {
        let Some(node) = self.node(id) else {
            return;
        };
        let mut cold = node.cold_write();
        cold.healthy = false;
        cold.success_streak = 0;
        cold.unhealthy_reason = reason.map(Arc::from);
    }

    /// Immediate bench for missing / public URLs (fail streak at threshold).
    pub fn mark_unreachable(&self, id: &NodeId, reason: &str) {
        let Some(node) = self.node(id) else {
            return;
        };
        let threshold = self.health.fail_streak_threshold;
        let _ = node
            .fail_streak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.max(threshold))
            });
        let mut cold = node.cold_write();
        cold.healthy = false;
        cold.success_streak = 0;
        cold.unhealthy_reason = Some(Arc::from(reason));
        cold.probe_backoff = self.health.interval_seconds;
        cold.next_probe_at =
            Instant::now() + Duration::from_secs_f64(self.health.interval_seconds.max(0.05));
    }

    /// Record a successful `/api/tags` probe. `success_threshold` gates recovery.
    pub fn note_probe_success(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        let threshold = self.health.success_threshold;
        node.fail_streak.store(0, Ordering::Relaxed);
        let mut cold = node.cold_write();
        cold.success_streak = cold.success_streak.saturating_add(1);
        if !cold.healthy && cold.success_streak >= threshold {
            cold.healthy = true;
            cold.unhealthy_reason = None;
            cold.probe_backoff = 0.0;
        }
    }

    /// Record a failed `/api/tags` probe. Unhealthy once fail streak hits threshold.
    pub fn note_probe_failure(&self, id: &NodeId, reason: &str) {
        let Some(node) = self.node(id) else {
            return;
        };
        let threshold = self.health.fail_streak_threshold;
        let interval = self.health.interval_seconds;
        let backoff_max = self.health.backoff_max_seconds;
        let new_fail = node
            .fail_streak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(1))
            })
            .map_or(0, |prev| prev.saturating_add(1));
        let mut cold = node.cold_write();
        cold.success_streak = 0;
        if cold.healthy && new_fail >= threshold {
            cold.healthy = false;
            cold.unhealthy_reason = Some(Arc::from(reason));
        }
        if !cold.healthy {
            let doubled = if cold.probe_backoff > 0.0 {
                cold.probe_backoff * 2.0
            } else {
                interval
            };
            cold.probe_backoff = doubled.max(interval).min(backoff_max);
        } else {
            cold.probe_backoff = 0.0;
        }
    }

    /// Schedule the next probe instant (health loop + tests).
    pub fn set_next_probe_at(&self, id: &NodeId, at: Instant) {
        if let Some(node) = self.node(id) {
            node.cold_write().next_probe_at = at;
        }
    }

    /// When the next probe is due.
    pub fn next_probe_at(&self, id: &NodeId) -> Option<Instant> {
        self.node(id).map(|n| n.cold_read().next_probe_at)
    }

    /// Current backoff delay in seconds (0 while healthy).
    pub fn probe_backoff(&self, id: &NodeId) -> f64 {
        self.node(id)
            .map(|n| n.cold_read().probe_backoff)
            .unwrap_or(0.0)
    }

    /// Replace the on-disk model set reported by `/api/tags`.
    ///
    /// Drops tag records for names no longer present. Name-only updates keep
    /// existing records for surviving names.
    pub fn update_models(&self, id: &NodeId, models: impl IntoIterator<Item = impl AsRef<str>>) {
        let Some(node) = self.node(id) else {
            return;
        };
        let names: HashSet<String> = models
            .into_iter()
            .map(|m| normalize_model(m.as_ref()))
            .filter(|m| !m.is_empty())
            .collect();
        let mut cold = node.cold_write();
        let mut records = (*cold.tag_records).clone();
        records.retain(|name, _| names.contains(name));
        cold.models = Arc::new(names);
        cold.tag_records = Arc::new(records);
    }

    /// Replace names and list records from a tags probe.
    pub fn update_models_from_records(
        &self,
        id: &NodeId,
        records: impl IntoIterator<Item = (impl AsRef<str>, TagRecord)>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        let mut names = HashSet::new();
        let mut map = HashMap::new();
        for (name, record) in records {
            let name = normalize_model(name.as_ref());
            if name.is_empty() {
                continue;
            }
            names.insert(name.clone());
            map.insert(name, record);
        }
        let mut cold = node.cold_write();
        cold.models = Arc::new(names);
        cold.tag_records = Arc::new(map);
    }

    /// Record live `/api/ps` state (tests inject warmth with default records).
    pub fn update_ps_state(
        &self,
        id: &NodeId,
        loaded_models: impl IntoIterator<Item = impl AsRef<str>>,
        loaded_vram_gb: Option<f64>,
    ) {
        self.update_ps_from_records(
            id,
            loaded_models.into_iter().map(|m| (m, PsRecord::default())),
            loaded_vram_gb,
        );
    }

    /// Replace loaded names and per-model `/api/ps` probe records for a node.
    pub fn update_ps_from_records(
        &self,
        id: &NodeId,
        records: impl IntoIterator<Item = (impl AsRef<str>, PsRecord)>,
        loaded_vram_gb: Option<f64>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        let mut names = HashSet::new();
        let mut map = HashMap::new();
        for (name, record) in records {
            let name = normalize_model(name.as_ref());
            if name.is_empty() {
                continue;
            }
            names.insert(name.clone());
            map.insert(name, record);
        }
        let loaded = Arc::new(names);
        {
            let mut cold = node.cold_write();
            cold.loaded_models = Arc::clone(&loaded);
            cold.ps_records = Arc::new(map);
            cold.loaded_vram_gb = loaded_vram_gb;
            remarge(&mut cold);
        }
        reconcile_ps_reservations(&node, &loaded);
    }

    /// Admit a client inference forward (native generate/chat/embed and OpenAI
    /// `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`).
    /// Writes `last_client_request_at`.
    ///
    /// Refuses a missing node, a draining or forget-pending Verda node, or a
    /// node already at `max_inflight_effective`. Permanent draining still
    /// admits so in-flight inventory removes can drain to zero.
    pub fn inflight_inc(&self, id: &NodeId) -> InflightAdmit {
        let Some(node) = self.node(id) else {
            return InflightAdmit::Missing;
        };
        if matches!(node.origin, NodeOrigin::Verda | NodeOrigin::Runpod)
            && (node.draining.load(Ordering::Acquire)
                || node.forget_pending.load(Ordering::Acquire))
        {
            return InflightAdmit::Draining;
        }
        let cap = {
            let cold = node.cold_read();
            effective_max_inflight(
                cold.max_inflight,
                self.policy.default_max_inflight,
                cold.capacity_effective.vram_gb(),
                cold.pressure_level,
            )
        };
        loop {
            let cur = node.inflight.load(Ordering::Acquire);
            if cur >= cap {
                return InflightAdmit::Saturated;
            }
            match node.inflight.compare_exchange_weak(
                cur,
                cur.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    node.last_client_ms.store(now_ms(), Ordering::Relaxed);
                    return InflightAdmit::Admitted;
                }
                Err(_) => continue,
            }
        }
    }

    /// Release one inflight slot. Draining permanent nodes with zero inflight are dropped.
    pub fn inflight_dec(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if saturating_fetch_sub(&node.inflight) == 0 {
            self.remove_if_droppable(id, &node);
        }
        self.slot_notify.notify_waiters();
    }

    /// Future that resolves when an inflight slot may have freed (saturation wait).
    pub fn slot_notified(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.slot_notify.notified()
    }

    /// Occupy an inflight slot without touching idle (warm-keeper only).
    ///
    /// Uncapped: the warm keeper already self-limits via
    /// `model_warm_max_inflight_ratio`.
    pub fn occupancy_inc(&self, id: &NodeId) {
        if let Some(node) = self.node(id) {
            saturating_fetch_add(&node.inflight);
        }
    }

    /// Release a warm-keeper occupancy slot. Sweeps drained nodes.
    pub fn occupancy_dec(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if saturating_fetch_sub(&node.inflight) == 0 {
            self.remove_if_droppable(id, &node);
        }
        self.slot_notify.notify_waiters();
    }

    /// In-process idle timestamp of last client inference forward.
    pub fn last_client_request_at(&self, id: &NodeId) -> Option<Instant> {
        self.node(id).and_then(|n| n.last_client_request_at())
    }

    /// Current inflight count.
    pub fn inflight(&self, id: &NodeId) -> u32 {
        self.node(id)
            .map(|n| n.inflight.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Current fail streak.
    pub fn fail_streak(&self, id: &NodeId) -> u32 {
        self.node(id)
            .map(|n| n.fail_streak.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Current reserved VRAM.
    pub fn reserved_vram_gb(&self, id: &NodeId) -> f64 {
        self.node(id)
            .map(|n| load_f64(&n.reserved_vram_bits))
            .unwrap_or(0.0)
    }

    /// Clear the fail streak after a successful upstream response.
    pub fn mark_request_success(&self, id: &NodeId) {
        if let Some(node) = self.node(id) {
            node.fail_streak.store(0, Ordering::Relaxed);
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
        let Some(node) = self.node(id) else {
            return;
        };
        let new_fail = node
            .fail_streak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(credit))
            })
            .map_or(0, |prev| prev.saturating_add(credit));
        if new_fail < self.health.fail_streak_threshold {
            return;
        }
        let mut cold = node.cold_write();
        if cold.healthy {
            cold.healthy = false;
            cold.success_streak = 0;
            cold.unhealthy_reason = Some(Arc::from("probe_failures"));
        }
    }

    /// Reserve VRAM for a cold load. `None` when estimate is 0 or the node is unknown.
    pub fn reserve_vram(&self, id: &NodeId, model: &str, estimate_gb: f64) -> Option<u64> {
        if estimate_gb <= 0.0 {
            return None;
        }
        let node = self.node(id)?;
        let mut ledger = node.ledger();
        let reservation_id = ledger.next_id;
        ledger.next_id = ledger.next_id.saturating_add(1);
        ledger
            .vram
            .insert(reservation_id, (normalize_model(model), estimate_gb));
        ledger.reserved_vram_gb += estimate_gb;
        node.sync_reserved(&ledger);
        Some(reservation_id)
    }

    /// Release a VRAM reservation (no-op for `None`).
    pub fn release_vram(&self, id: &NodeId, reservation_id: Option<u64>) {
        let Some(reservation_id) = reservation_id else {
            return;
        };
        let Some(node) = self.node(id) else {
            return;
        };
        let mut ledger = node.ledger();
        if let Some((_, estimate)) = ledger.vram.remove(&reservation_id) {
            ledger.reserved_vram_gb = (ledger.reserved_vram_gb - estimate).max(0.0);
            node.sync_reserved(&ledger);
        }
    }

    /// Reserve system RAM for a cold load.
    pub fn reserve_ram(&self, id: &NodeId, model: &str, estimate_gb: f64) -> Option<u64> {
        if estimate_gb <= 0.0 {
            return None;
        }
        let node = self.node(id)?;
        let mut ledger = node.ledger();
        let reservation_id = ledger.next_id;
        ledger.next_id = ledger.next_id.saturating_add(1);
        ledger
            .ram
            .insert(reservation_id, (normalize_model(model), estimate_gb));
        ledger.reserved_ram_gb += estimate_gb;
        node.sync_reserved(&ledger);
        Some(reservation_id)
    }

    /// Release a RAM reservation (no-op for `None`).
    pub fn release_ram(&self, id: &NodeId, reservation_id: Option<u64>) {
        let Some(reservation_id) = reservation_id else {
            return;
        };
        let Some(node) = self.node(id) else {
            return;
        };
        let mut ledger = node.ledger();
        if let Some((_, estimate)) = ledger.ram.remove(&reservation_id) {
            ledger.reserved_ram_gb = (ledger.reserved_ram_gb - estimate).max(0.0);
            node.sync_reserved(&ledger);
        }
    }

    /// Store agent-reported pressure (soft-fail capacity probe). Does not change health.
    pub fn set_pressure_level(&self, id: &NodeId, level: PressureLevel) {
        if let Some(node) = self.node(id) {
            node.cold_write().pressure_level = level;
        }
    }

    /// Merge a successful node-agent report. Never flips health.
    pub fn apply_capacity_report(
        &self,
        id: &NodeId,
        report: &CapacityReport,
        pressure_level: Option<PressureLevel>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        let mut cold = node.cold_write();
        let mut discovered = report.as_capacity();
        if discovered
            .vram_gb
            .is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            discovered.vram_gb = None;
        }
        if discovered.ram_gb.is_some_and(|v| !v.is_finite() || v < 0.0) {
            discovered.ram_gb = None;
        }
        cold.capacity_discovered = Some(discovered);
        cold.vram_free_gb = finite_or_none(report.vram_free_gb);
        cold.vram_free_known = report.vram_free_is_known() && report.vram_free_gb.is_finite();
        cold.vram_used_gb = finite_or_none(report.vram_used_gb);
        cold.vram_used_known = report.vram_used_is_known() && report.vram_used_gb.is_finite();
        cold.gpu_backend = report.gpu_backend.unwrap_or_default();
        let utils: Vec<f64> = report
            .gpus_detail
            .iter()
            .filter_map(|gpu| finite_opt(gpu.utilization_gpu_pct))
            .collect();
        if utils.is_empty() {
            cold.gpu_util_pct = None;
            cold.gpu_util_known = false;
        } else {
            cold.gpu_util_pct = Some(utils.iter().sum::<f64>() / utils.len() as f64);
            cold.gpu_util_known = true;
        }
        cold.cpu_usage_pct = finite_opt(report.cpu_usage_pct).or_else(|| {
            report
                .pressure
                .as_ref()
                .and_then(|pressure| finite_opt(pressure.cpu_usage_pct))
        });
        cold.ollama_running = report.ollama_running;
        cold.loaded_model_count = report.loaded_model_count.map(|n| n.max(0) as u32);
        cold.disk_total_gb = finite_opt(report.disk_total_gb);
        cold.disk_available_gb = finite_opt(report.disk_available_gb);
        cold.gpus_detail = report
            .gpus_detail
            .iter()
            .take(MAX_GPU_METRIC_ROWS)
            .map(GpuSnapshot::from_detail)
            .collect::<Vec<_>>()
            .into();
        if let Some(pressure) = report.pressure.as_ref() {
            cold.ram_available_gb = finite_opt(pressure.ram_available_gb);
            cold.ram_available_ratio = finite_opt(pressure.ram_available_ratio);
        }
        if let Some(level) = pressure_level {
            cold.pressure_level = level;
        }
        cold.capacity_error = None;
        remarge(&mut cold);
    }

    /// Allowlisted capacity miss (`http_status` / `timeout` / `unreachable` / `parse`).
    pub fn set_capacity_error(&self, id: &NodeId, reason: &str) {
        if let Some(node) = self.node(id) {
            node.cold_write().capacity_error = Some(Arc::from(reason));
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
        let Some(node) = self.node(id) else {
            return;
        };
        if node.origin != NodeOrigin::Permanent {
            return;
        }
        node.draining.store(true, Ordering::Release);
        self.remove_if_droppable(id, &node);
    }

    /// Diff fleet.yaml membership by `NodeId`. Never deletes Verda rows.
    pub fn apply_permanent_inventory(&self, nodes: &[NodeConfig]) {
        let mut inner = self.write();
        let incoming: HashSet<NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
        let stale: Vec<NodeId> = inner
            .nodes
            .iter()
            .filter(|(_, n)| n.origin == NodeOrigin::Permanent && !incoming.contains(&n.id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            begin_remove_permanent_locked(&mut inner, &id);
        }
        for node in nodes {
            upsert_permanent_locked(&mut inner, node.clone());
        }
        sweep_drained_locked(&mut inner);
    }

    /// Insert or refresh an adopt/debug node. Does not overwrite permanent or cloud rows.
    pub fn upsert_adopt(&self, config: NodeConfig) {
        let mut inner = self.write();
        let id = config.id.clone();
        match inner.nodes.get(&id) {
            Some(existing) if existing.origin == NodeOrigin::Adopt => {
                existing.apply_permanent_config(config);
            }
            Some(_) => {}
            None => {
                inner
                    .nodes
                    .insert(id, Arc::new(Node::from_config(config, NodeOrigin::Adopt)));
            }
        }
    }

    /// Insert or refresh a Verda ephemeral (tests / future manager). Does not overwrite Permanent.
    pub fn upsert_verda(&self, config: NodeConfig) {
        let mut inner = self.write();
        let id = config.id.clone();
        match inner.nodes.get(&id) {
            Some(existing) if existing.origin == NodeOrigin::Verda => {
                existing.apply_permanent_config(config);
            }
            Some(_) => {}
            None => {
                inner
                    .nodes
                    .insert(id, Arc::new(Node::from_config(config, NodeOrigin::Verda)));
            }
        }
    }

    /// Insert or refresh a RunPod ephemeral. Does not overwrite Permanent or Verda.
    pub fn upsert_runpod(&self, config: NodeConfig) {
        let mut inner = self.write();
        let id = config.id.clone();
        match inner.nodes.get(&id) {
            Some(existing) if existing.origin == NodeOrigin::Runpod => {
                existing.apply_permanent_config(config);
            }
            Some(_) => {}
            None => {
                inner
                    .nodes
                    .insert(id, Arc::new(Node::from_config(config, NodeOrigin::Runpod)));
            }
        }
    }

    /// Forget a Verda node. Permanent hosts are ignored.
    ///
    /// Sets forget-pending (not the same as idle-destroy `draining`). Drops the
    /// row immediately when inflight is 0; otherwise ranking skips it and
    /// `inflight_dec` / `occupancy_dec` sweep when the counter hits zero.
    /// Failed-destroy draining Verda rows are not forget-pending and stay.
    pub fn remove_verda(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.origin != NodeOrigin::Verda {
            return;
        }
        node.forget_pending.store(true, Ordering::Release);
        node.draining.store(true, Ordering::Release);
        self.remove_if_droppable(id, &node);
    }

    /// Forget a RunPod node. Permanent / Verda hosts are ignored.
    pub fn remove_runpod(&self, id: &NodeId) {
        let Some(node) = self.node(id) else {
            return;
        };
        if node.origin != NodeOrigin::Runpod {
            return;
        }
        node.forget_pending.store(true, Ordering::Release);
        node.draining.store(true, Ordering::Release);
        self.remove_if_droppable(id, &node);
    }

    /// Mark a cloud node draining so ranking stops selecting it. No-op for
    /// permanent hosts (`begin_remove_permanent` owns that path).
    pub fn set_draining(&self, id: &NodeId, draining: bool) -> bool {
        let Some(node) = self.node(id) else {
            return false;
        };
        if !matches!(node.origin, NodeOrigin::Verda | NodeOrigin::Runpod) {
            return false;
        }
        node.draining.store(draining, Ordering::Release);
        true
    }

    /// Operator cordon (admin drain/undrain). Excludes the node from ranking and
    /// placement without destroying it. Idempotent; returns false when unknown.
    pub fn set_cordoned(&self, id: &NodeId, cordoned: bool) -> bool {
        let Some(node) = self.node(id) else {
            return false;
        };
        node.cordoned.store(cordoned, Ordering::Release);
        true
    }

    /// Drop permanent nodes that are draining with zero inflight.
    ///
    /// Completion paths already remove the decremented node in O(1). The health
    /// supervisor calls this as a safety net, not on every request.
    pub fn sweep_drained(&self) {
        let mut inner = self.write();
        sweep_drained_locked(&mut inner);
    }

    /// Sticky-affinity owner from an existing snapshot: prefer a warm healthy holder of `model`.
    pub fn sticky_owner_from(nodes: &[NodeSnapshot], model: &str) -> Option<NodeId> {
        let holders: Vec<&NodeSnapshot> = nodes
            .iter()
            .filter(|n| n.healthy && !n.draining && !n.cordoned && n.has_model(model))
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

    /// Sticky-affinity owner: prefer a warm healthy holder of `model`.
    pub fn sticky_owner(&self, model: &str) -> Option<NodeId> {
        Self::sticky_owner_from(&self.snapshot(), model)
    }

    /// Healthy nodes' on-disk models, grouped for `/api/tags`.
    pub fn aggregated_tags(&self) -> Vec<AggregatedTag> {
        let inner = self.read();
        let mut owned: Vec<(NodeId, HashSet<String>, HashMap<String, TagRecord>)> = Vec::new();
        for node in inner.nodes.values() {
            let draining = node.draining.load(Ordering::Acquire);
            let cold = node.cold_read();
            if !cold.healthy || draining {
                continue;
            }
            owned.push((
                node.id.clone(),
                (*cold.models).clone(),
                (*cold.tag_records).clone(),
            ));
        }
        drop(inner);
        merge_catalog(owned.iter().map(|(id, models, records)| CatalogNode {
            id,
            models,
            records,
        }))
    }

    /// Healthy nodes' loaded models for `/api/ps` (one row per node × model).
    pub fn aggregated_ps(&self) -> Vec<AggregatedPs> {
        let inner = self.read();
        let mut owned: Vec<(NodeId, HashMap<String, PsRecord>)> = Vec::new();
        for node in inner.nodes.values() {
            let draining = node.draining.load(Ordering::Acquire);
            let cold = node.cold_read();
            if !cold.healthy || draining {
                continue;
            }
            owned.push((node.id.clone(), (*cold.ps_records).clone()));
        }
        drop(inner);
        merge_ps(owned.iter().map(|(id, records)| PsNode { id, records }))
    }

    /// Pull size estimate in bytes: target node's tags-probe `size`, else catalog.
    ///
    /// Missing or `0` size yields `None` (do not skip for disk).
    pub fn pull_size_estimate_bytes(&self, node_id: &NodeId, model: &str) -> Option<u64> {
        let model = normalize_model(model);
        if model.is_empty() {
            return None;
        }
        let node_size = self.node(node_id).and_then(|node| {
            let cold = node.cold_read();
            cold.tag_records
                .get(&model)
                .and_then(|rec| rec.size)
                .filter(|&s| s > 0)
        });
        let catalog_size = self
            .aggregated_tags()
            .into_iter()
            .find(|tag| tag.name == model)
            .and_then(|tag| tag.size)
            .filter(|&s| s > 0);
        crate::routing::pull_size_bytes(node_size, catalog_size)
    }
}

fn remarge(cold: &mut Cold) {
    let merged = merge_capacity(
        &cold.static_capacity,
        cold.capacity_discovered.as_ref(),
        cold.loaded_vram_gb,
    );
    cold.capacity_effective = merged.capacity;
    cold.capacity_source = merged.source;
}

fn upsert_permanent_locked(inner: &mut Inner, config: NodeConfig) {
    let id = config.id.clone();
    match inner.nodes.get(&id) {
        Some(existing) if matches!(existing.origin, NodeOrigin::Verda | NodeOrigin::Adopt) => {
            tracing::warn!(
                node = %id,
                origin = existing.origin.as_str(),
                "fleet.yaml id collides with a non-permanent node; leaving row in place"
            );
        }
        Some(existing) => {
            existing.apply_permanent_config(config);
        }
        None => {
            inner.nodes.insert(
                id,
                Arc::new(Node::from_config(config, NodeOrigin::Permanent)),
            );
        }
    }
}

fn begin_remove_permanent_locked(inner: &mut Inner, id: &NodeId) {
    let Some(node) = inner.nodes.get(id) else {
        return;
    };
    if node.origin != NodeOrigin::Permanent {
        return;
    }
    node.draining.store(true, Ordering::Release);
}

fn sweep_drained_locked(inner: &mut Inner) {
    inner.nodes.retain(|_, node| !node.is_droppable());
}

fn reconcile_ps_reservations(node: &Node, loaded_models: &HashSet<String>) {
    let mut ledger = node.ledger();
    let stale: Vec<u64> = ledger
        .vram
        .iter()
        .filter(|(_, (model, _))| models_match(loaded_models, model))
        .map(|(id, _)| *id)
        .collect();
    for id in stale {
        if let Some((_, estimate)) = ledger.vram.remove(&id) {
            ledger.reserved_vram_gb = (ledger.reserved_vram_gb - estimate).max(0.0);
        }
    }
    let stale_ram: Vec<u64> = ledger
        .ram
        .iter()
        .filter(|(_, (model, _))| models_match(loaded_models, model))
        .map(|(id, _)| *id)
        .collect();
    for id in stale_ram {
        if let Some((_, estimate)) = ledger.ram.remove(&id) {
            ledger.reserved_ram_gb = (ledger.reserved_ram_gb - estimate).max(0.0);
        }
    }
    node.sync_reserved(&ledger);
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

    fn two_node_registry() -> Registry {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1), node("b", 24.0, 1)],
            ..Default::default()
        };
        Registry::new(&config)
    }

    #[test]
    fn aggregated_tags_omits_unhealthy_nodes() {
        let registry = two_node_registry();
        registry.set_healthy(&nid("a"));
        registry.update_models(&nid("a"), ["llama3.2:1b"]);
        registry.update_models(&nid("b"), ["llama3.2:1b"]);
        let rows = registry.aggregated_tags();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nodes, vec!["a".to_string()]);
        assert!(rows[0].digest.len() >= 12);
    }

    #[test]
    fn aggregated_ps_one_row_per_healthy_node_model() {
        let registry = two_node_registry();
        registry.set_healthy(&nid("a"));
        registry.set_healthy(&nid("b"));
        registry.update_ps_from_records(
            &nid("a"),
            [(
                "qwen3:8b",
                crate::fleet::PsRecord {
                    digest: "aaaaaaaaaaaa".into(),
                    size: Some(100),
                    size_vram: Some(80),
                    details: None,
                    expires_at: None,
                    context_length: Some(8192),
                },
            )],
            Some(1.0),
        );
        registry.update_ps_from_records(
            &nid("b"),
            [(
                "qwen3:8b",
                crate::fleet::PsRecord {
                    digest: "bbbbbbbbbbbb".into(),
                    size: Some(100),
                    size_vram: Some(90),
                    details: None,
                    expires_at: None,
                    context_length: None,
                },
            )],
            Some(2.0),
        );
        let rows = registry.aggregated_ps();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "qwen3:8b");
        assert_eq!(rows[0].node, "a");
        assert_eq!(rows[1].node, "b");
        assert!(rows.iter().all(|r| r.digest.len() >= 12));
    }

    #[test]
    fn aggregated_ps_omits_unhealthy_nodes() {
        let registry = two_node_registry();
        registry.set_healthy(&nid("a"));
        registry.update_ps_state(&nid("a"), ["llama3.2:1b"], Some(1.0));
        registry.update_ps_state(&nid("b"), ["llama3.2:1b"], Some(1.0));
        let rows = registry.aggregated_ps();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].node, "a");
    }

    #[test]
    fn update_models_from_records_replaces_catalog_and_prunes() {
        let registry = two_node_registry();
        let a = nid("a");
        registry.set_healthy(&a);
        registry.update_models_from_records(
            &a,
            [
                (
                    "llama3.2:1b",
                    TagRecord {
                        digest: "aaaaaaaaaaaa".into(),
                        size: Some(10),
                        modified_at: Some("2026-01-01T00:00:00Z".into()),
                        details: None,
                        capabilities: None,
                    },
                ),
                (
                    "qwen3:4b",
                    TagRecord {
                        digest: "bbbbbbbbbbbb".into(),
                        size: Some(20),
                        modified_at: None,
                        details: None,
                        capabilities: None,
                    },
                ),
            ],
        );
        registry.update_models(&a, ["llama3.2:1b"]);
        let rows = registry.aggregated_tags();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "llama3.2:1b");
        assert_eq!(rows[0].digest, "aaaaaaaaaaaa");
        assert_eq!(rows[0].size, Some(10));
    }

    #[test]
    fn aggregated_tags_name_only_uses_placeholder_digest() {
        let registry = two_node_registry();
        registry.set_healthy(&nid("a"));
        registry.update_models(&nid("a"), ["llama3.2:1b"]);
        let digest = registry.aggregated_tags()[0].digest.clone();
        assert_eq!(digest.len(), 64);
        assert_eq!(
            digest,
            crate::fleet::tags::placeholder_digest("llama3.2:1b")
        );
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
    fn inflight_inc_refuses_draining_verda_but_admits_permanent() {
        let config = RouterConfig {
            nodes: vec![node("desk", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        registry.upsert_verda(node("spot", 24.0, 1));
        let spot = nid("spot");
        let desk = nid("desk");
        assert!(registry.set_draining(&spot, true));
        assert!(!registry.set_draining(&desk, true));
        assert_eq!(registry.inflight_inc(&spot), InflightAdmit::Draining);
        assert_eq!(registry.inflight(&spot), 0);
        assert_eq!(registry.inflight_inc(&desk), InflightAdmit::Admitted);
        assert_eq!(registry.inflight(&desk), 1);
        assert!(registry.set_draining(&spot, false));
        assert_eq!(registry.inflight_inc(&spot), InflightAdmit::Admitted);
        assert_eq!(registry.inflight(&spot), 1);
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
        assert_eq!(snap_a.labels.as_ref(), &["gpu".to_string()][..]);
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
    fn upsert_runpod_sets_runpod_origin() {
        let registry = Registry::new(&RouterConfig::default());
        registry.upsert_runpod(node("runpod-pod-1", 24.0, 1));
        let snap = registry.get(&nid("runpod-pod-1")).unwrap();
        assert_eq!(snap.origin, NodeOrigin::Runpod);
        assert_eq!(snap.origin.as_str(), "runpod");
    }

    #[test]
    fn upsert_adopt_sets_adopt_origin_and_does_not_count_as_verda() {
        let registry = Registry::new(&RouterConfig::default());
        registry.upsert_adopt(node("verda-like-desk", 8.0, 1));
        let snap = registry.get(&nid("verda-like-desk")).unwrap();
        assert_eq!(snap.origin, NodeOrigin::Adopt);
        assert_eq!(snap.origin.as_str(), "adopt");
        let verda_n = registry
            .snapshot()
            .iter()
            .filter(|n| n.origin == NodeOrigin::Verda)
            .count();
        assert_eq!(verda_n, 0);
        registry.upsert_verda(node("verda-real", 24.0, 1));
        registry.upsert_adopt(node("verda-real", 8.0, 1));
        assert_eq!(
            registry.get(&nid("verda-real")).unwrap().origin,
            NodeOrigin::Verda
        );
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
    fn set_node_url_refuses_public_ip() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        let err = registry
            .set_node_url(&id, "http://8.8.8.8:11434")
            .expect_err("public");
        assert!(err.contains("public IP"));
        assert_eq!(
            registry.get(&id).unwrap().url.as_deref(),
            Some("http://a:11434")
        );
        let err = registry
            .set_node_url(&id, "http://100.64.0.9:11434")
            .expect_err("cgnat");
        assert!(err.contains("public IP"));
        let err = registry
            .set_node_url(&id, "http://[2606:4700:4700::1111]:11434")
            .expect_err("public v6");
        assert!(err.contains("public IP"));
        let err = registry
            .set_node_url(&id, "http://[::ffff:8.8.8.8]:11434")
            .expect_err("mapped");
        assert!(err.contains("public IP"));
        let err = registry
            .set_node_url(&id, "http://0.0.0.0:11434")
            .expect_err("unspecified");
        assert!(err.contains("public IP"));
        registry
            .set_node_url(&id, "http://10.0.0.9:11434")
            .expect("rfc1918");
        assert_eq!(
            registry.get(&id).unwrap().url.as_deref(),
            Some("http://10.0.0.9:11434")
        );
        assert!(!registry.get(&id).unwrap().healthy);
        registry
            .set_node_url(&id, "http://[::1]:11434")
            .expect("v6 loopback");
    }

    #[test]
    fn set_node_url_refuses_public_share_hostname() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("a");
        let err = registry
            .set_node_url(&id, "https://abc.share.zrok.io")
            .expect_err("public share");
        assert!(err.contains("public share"));
        assert_eq!(
            registry.get(&id).unwrap().url.as_deref(),
            Some("http://a:11434")
        );
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

    #[test]
    fn apply_capacity_report_persists_rocm_fixture() {
        let config = RouterConfig {
            nodes: vec![node("rocm", 32.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let id = nid("rocm");
        let report: CapacityReport =
            serde_json::from_str(include_str!("../../tests/fixtures/capacity-rocm.json"))
                .expect("rocm fixture");
        registry.apply_capacity_report(&id, &report, Some(PressureLevel::Ok));
        let snap = registry.get(&id).unwrap();
        assert_eq!(snap.gpu_backend, GpuBackend::Rocm);
        assert!(snap.vram_free_known);
        assert!(snap.vram_used_known);
        assert!((snap.vram_free_gb.unwrap() - 31.0).abs() < 1e-9);
        assert_eq!(snap.gpus_detail.len(), 1);
        assert!(!snap.gpu_util_known);
    }

    #[test]
    fn concurrent_inflight_and_reserve_do_not_wrap() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1), node("b", 8.0, 1)],
            ..Default::default()
        };
        let registry = Arc::new(Registry::new(&config));
        let a = nid("a");
        let b = nid("b");
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let registry = Arc::clone(&registry);
                let a = a.clone();
                let b = b.clone();
                scope.spawn(move || {
                    for _i in 0..200 {
                        if registry.inflight_inc(&a).admitted() {
                            let _ = registry.snapshot();
                            let rid = registry.reserve_vram(&a, "llama", 1.0);
                            registry.release_vram(&a, rid);
                            registry.inflight_dec(&a);
                        }
                        registry.occupancy_inc(&b);
                        registry.occupancy_dec(&b);
                    }
                });
            }
        });
        assert_eq!(registry.inflight(&a), 0);
        assert_eq!(registry.inflight(&b), 0);
        assert!(registry.last_client_request_at(&a).is_some());
        assert!(registry.last_client_request_at(&b).is_none());
        assert_eq!(registry.reserved_vram_gb(&a), 0.0);
    }

    #[test]
    fn inflight_inc_cas_respects_cap() {
        let config = RouterConfig {
            nodes: vec![node("a", 8.0, 1)],
            ..Default::default()
        };
        let registry = Arc::new(Registry::new(&config));
        let id = nid("a");
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let registry = Arc::clone(&registry);
                let id = id.clone();
                let outcomes = Arc::clone(&outcomes);
                scope.spawn(move || {
                    let admit = registry.inflight_inc(&id);
                    outcomes.lock().unwrap().push(admit);
                });
            }
        });
        let outcomes = outcomes.lock().unwrap();
        let admitted = outcomes
            .iter()
            .filter(|a| **a == InflightAdmit::Admitted)
            .count();
        let saturated = outcomes
            .iter()
            .filter(|a| **a == InflightAdmit::Saturated)
            .count();
        assert_eq!(admitted, 2);
        assert_eq!(saturated, 14);
        assert_eq!(registry.inflight(&id), 2);
    }

    #[test]
    fn remove_verda_with_inflight_tombstones_then_drops() {
        let registry = Registry::new(&RouterConfig::default());
        registry.upsert_verda(node("spot", 24.0, 1));
        let spot = nid("spot");
        registry.set_healthy(&spot);
        assert_eq!(registry.inflight_inc(&spot), InflightAdmit::Admitted);
        registry.remove_verda(&spot);
        let snap = registry.get(&spot).expect("tombstone");
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
        assert!(ranked.ranked.iter().all(|n| n.id.as_str() != "spot"));
        registry.inflight_dec(&spot);
        assert!(registry.get(&spot).is_none());
    }

    #[test]
    fn set_draining_does_not_drop_idle_verda() {
        let registry = Registry::new(&RouterConfig::default());
        registry.upsert_verda(node("spot", 24.0, 1));
        let spot = nid("spot");
        assert!(registry.set_draining(&spot, true));
        assert!(registry.get(&spot).is_some());
        registry.sweep_drained();
        assert!(registry.get(&spot).is_some());
    }

    #[test]
    fn apply_capacity_report_drops_non_finite_ratio() {
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
            ..CapacityReport::default()
        };
        report.pressure = Some(crate::capacity::Pressure {
            ram_available_gb: Some(24.0),
            ram_available_ratio: Some(f64::NAN),
            ..Default::default()
        });
        registry.apply_capacity_report(&id, &report, Some(PressureLevel::Ok));
        let snap = registry.get(&id).unwrap();
        assert!(snap.ram_available_ratio.is_none());
        assert_eq!(snap.ram_available_gb, Some(24.0));
    }

    #[test]
    fn cordon_survives_inventory_reload_and_excludes_from_rank() {
        let config = RouterConfig {
            nodes: vec![node("desk", 8.0, 1), node("spare", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let desk = nid("desk");
        let spare = nid("spare");
        registry.set_healthy(&desk);
        registry.set_healthy(&spare);
        registry.update_models(&desk, ["llama3.2:3b"]);
        registry.update_models(&spare, ["llama3.2:3b"]);
        assert!(registry.set_cordoned(&desk, true));
        assert!(registry.get(&desk).unwrap().cordoned);
        assert!(!registry.get(&desk).unwrap().draining);

        registry.apply_permanent_inventory(&[node("desk", 8.0, 1), node("spare", 8.0, 1)]);
        let snap = registry.get(&desk).unwrap();
        assert!(snap.cordoned);
        assert!(!snap.draining);
        assert!(registry.get(&desk).is_some());

        let ranked = crate::routing::rank_nodes(
            &registry.snapshot(),
            crate::routing::RequestClass::Small,
            Some("llama3.2:3b"),
            registry.policy(),
            None,
            &HashSet::new(),
            0,
        );
        assert!(ranked.ranked.iter().all(|n| n.id.as_str() != "desk"));
        assert_eq!(ranked.ranked[0].id.as_str(), "spare");

        assert!(registry.set_cordoned(&desk, false));
        let ranked = crate::routing::rank_nodes(
            &registry.snapshot(),
            crate::routing::RequestClass::Small,
            Some("llama3.2:3b"),
            registry.policy(),
            None,
            &HashSet::new(),
            0,
        );
        assert!(ranked.ranked.iter().any(|n| n.id.as_str() == "desk"));
    }

    #[test]
    fn cordon_does_not_make_permanent_droppable() {
        let config = RouterConfig {
            nodes: vec![node("desk", 8.0, 1)],
            ..Default::default()
        };
        let registry = Registry::new(&config);
        let desk = nid("desk");
        assert!(registry.set_cordoned(&desk, true));
        registry.sweep_drained();
        assert!(registry.get(&desk).is_some());
        assert!(registry.get(&desk).unwrap().cordoned);
    }
}
