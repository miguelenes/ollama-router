//! Prometheus text exposition (`GET /metrics`). Binary-only; core/verda stay free of this crate.

use std::time::Duration;

use ollama_router_core::cloud::FleetEvents;
use ollama_router_core::fleet::{FleetState, NodeOrigin, PressureLevel, Registry};
use ollama_router_core::jobs::{JobKind, JobObserver, JobStatus};
use prometheus::{
    Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec,
    Opts, Registry as PromRegistry, TextEncoder,
};
use serde_json::{json, Value};

/// Live RED + fleet series. One custom [`PromRegistry`] per process state (tests stay isolated).
#[derive(Clone)]
pub struct Metrics {
    registry: PromRegistry,
    requests: IntCounterVec,
    duration: HistogramVec,
    inflight: GaugeVec,
    healthy: IntGaugeVec,
    vram_free: GaugeVec,
    vram_free_known: IntGaugeVec,
    vram: GaugeVec,
    vram_used: GaugeVec,
    vram_used_known: IntGaugeVec,
    pressure: IntGaugeVec,
    node_info: GaugeVec,
    route_reason: IntCounterVec,
    probe_duration: Histogram,
    verda_instances: IntGauge,
    verda_spot_price: Gauge,
    verda_events: IntCounterVec,
    job_operations: IntCounterVec,
    auto_pull_wait: IntCounterVec,
    aggregated_models: IntGauge,
    node_models: IntGaugeVec,
    discovery: IntCounterVec,
    ram_available: GaugeVec,
    ram_available_ratio: GaugeVec,
    ram_available_known: IntGaugeVec,
    ram_total: GaugeVec,
    gpu_util: GaugeVec,
    gpu_util_known: IntGaugeVec,
    cpu_usage: GaugeVec,
    cpu_usage_known: IntGaugeVec,
    loaded_models: IntGaugeVec,
    backend_info: GaugeVec,
    gpu_vram: GaugeVec,
    gpu_vram_free: GaugeVec,
    gpu_vram_free_known: IntGaugeVec,
    gpu_temp: GaugeVec,
    fail_streak: IntGaugeVec,
    draining: IntGaugeVec,
    max_inflight: IntGaugeVec,
    reserved_vram: GaugeVec,
    loaded_vram: GaugeVec,
    loaded_vram_known: IntGaugeVec,
    disk_available: GaugeVec,
    ollama_up: IntGaugeVec,
}

impl Metrics {
    /// Register SPEC series names. Names are compile-time constants.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = PromRegistry::new();
        let requests = IntCounterVec::new(
            Opts::new(
                "ollama_router_requests_total",
                "Completed proxy requests by class, status, and upstream node",
            ),
            &["class", "code", "node"],
        )?;
        let duration = HistogramVec::new(
            HistogramOpts::new(
                "ollama_router_request_duration_seconds",
                "Proxy wall time until response headers (no model label)",
            ),
            &["class"],
        )?;
        let inflight = GaugeVec::new(
            Opts::new(
                "ollama_router_inflight",
                "In-flight client forwards per node",
            ),
            &["node"],
        )?;
        let healthy = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_healthy",
                "Whether the node is healthy (1) or not (0)",
            ),
            &["node"],
        )?;
        let vram_free = GaugeVec::new(
            Opts::new(
                "ollama_router_node_vram_free_gb",
                "Reported free VRAM headroom per node (GiB). Gate on vram_free_known.",
            ),
            &["node"],
        )?;
        let vram_free_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_vram_free_known",
                "1 if vram_free_gb was measured (0 GiB free can be a full GPU)",
            ),
            &["node"],
        )?;
        let vram = GaugeVec::new(
            Opts::new(
                "ollama_router_node_vram_gb",
                "Effective VRAM capacity per node (GiB)",
            ),
            &["node"],
        )?;
        let vram_used = GaugeVec::new(
            Opts::new(
                "ollama_router_node_vram_used_gb",
                "Reported used VRAM per node (GiB). Gate on vram_used_known.",
            ),
            &["node"],
        )?;
        let vram_used_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_vram_used_known",
                "1 if vram_used_gb was measured",
            ),
            &["node"],
        )?;
        let pressure = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_pressure",
                "Capacity-agent pressure: 0 unknown, 1 ok, 2 elevated, 3 critical",
            ),
            &["node"],
        )?;
        let node_info = GaugeVec::new(
            Opts::new(
                "ollama_router_node_info",
                "Fleet node identity (1). origin=permanent|verda; role=cpu|gpu|verda|origin",
            ),
            &["node", "origin", "role"],
        )?;
        let route_reason = IntCounterVec::new(
            Opts::new(
                "ollama_router_route_reason_total",
                "Routing rejections and public-URL blocks",
            ),
            &["reason"],
        )?;
        let probe_duration = Histogram::with_opts(HistogramOpts::new(
            "ollama_router_probe_duration_seconds",
            "Health probe wall time",
        ))?;
        let verda_instances = IntGauge::new(
            "ollama_router_verda_instances",
            "Live registry nodes with origin Verda",
        )?;
        let verda_spot_price = Gauge::new(
            "ollama_router_verda_spot_price_per_hour",
            "Sum of known FleetState Verda spot prices",
        )?;
        let verda_events = IntCounterVec::new(
            Opts::new(
                "ollama_router_verda_events_total",
                "Verda fleet lifecycle events",
            ),
            &["event"],
        )?;
        let job_operations = IntCounterVec::new(
            Opts::new(
                "ollama_router_job_operations_total",
                "Terminal model-operation jobs",
            ),
            &["kind", "status"],
        )?;
        let auto_pull_wait = IntCounterVec::new(
            Opts::new(
                "ollama_router_auto_pull_wait_total",
                "auto_pull_on_miss wait outcomes (no model label)",
            ),
            &["outcome"],
        )?;
        let aggregated_models = IntGauge::new(
            "ollama_router_aggregated_models",
            "Unique models on healthy non-draining nodes (GET /api/tags and GET /v1/models)",
        )?;
        let node_models = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_models",
                "On-disk model count per node (no model-name label)",
            ),
            &["node"],
        )?;
        let discovery = IntCounterVec::new(
            Opts::new(
                "ollama_router_discovery_total",
                "Aggregated model-list requests (tags or openai_models)",
            ),
            &["endpoint"],
        )?;
        let ram_available = GaugeVec::new(
            Opts::new(
                "ollama_router_node_ram_available_gb",
                "Available RAM per node (GiB). Gate on ram_available_known.",
            ),
            &["node"],
        )?;
        let ram_available_ratio = GaugeVec::new(
            Opts::new(
                "ollama_router_node_ram_available_ratio",
                "Available RAM / total RAM (0-1)",
            ),
            &["node"],
        )?;
        let ram_available_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_ram_available_known",
                "1 if ram_available_gb was measured",
            ),
            &["node"],
        )?;
        let ram_total = GaugeVec::new(
            Opts::new(
                "ollama_router_node_ram_total_gb",
                "Effective RAM capacity per node (GiB)",
            ),
            &["node"],
        )?;
        let gpu_util = GaugeVec::new(
            Opts::new(
                "ollama_router_node_gpu_utilization_pct",
                "Mean GPU utilization percent. Gate on gpu_util_known.",
            ),
            &["node"],
        )?;
        let gpu_util_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_gpu_util_known",
                "1 if mean GPU utilization was measured",
            ),
            &["node"],
        )?;
        let cpu_usage = GaugeVec::new(
            Opts::new(
                "ollama_router_node_cpu_usage_pct",
                "Host CPU utilization percent. Gate on cpu_usage_known.",
            ),
            &["node"],
        )?;
        let cpu_usage_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_cpu_usage_known",
                "1 if cpu_usage_pct was measured (not a first-sample 0)",
            ),
            &["node"],
        )?;
        let loaded_models = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_loaded_models",
                "Loaded model count per node (no model-name label)",
            ),
            &["node"],
        )?;
        let backend_info = GaugeVec::new(
            Opts::new(
                "ollama_router_node_backend_info",
                "GPU backend identity (1). backend=cpu|cuda|rocm|metal|unknown",
            ),
            &["node", "backend"],
        )?;
        let gpu_vram = GaugeVec::new(
            Opts::new("ollama_router_node_gpu_vram_gb", "Per-GPU VRAM total (GiB)"),
            &["node", "gpu"],
        )?;
        let gpu_vram_free = GaugeVec::new(
            Opts::new(
                "ollama_router_node_gpu_vram_free_gb",
                "Per-GPU VRAM free (GiB). Gate on gpu_vram_free_known.",
            ),
            &["node", "gpu"],
        )?;
        let gpu_vram_free_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_gpu_vram_free_known",
                "1 if per-GPU free VRAM was measured",
            ),
            &["node", "gpu"],
        )?;
        let gpu_temp = GaugeVec::new(
            Opts::new(
                "ollama_router_node_gpu_temperature_c",
                "Per-GPU temperature Celsius when measured",
            ),
            &["node", "gpu"],
        )?;
        let fail_streak = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_fail_streak",
                "Consecutive health-probe failures",
            ),
            &["node"],
        )?;
        let draining = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_draining",
                "1 if the node is draining (inventory remove)",
            ),
            &["node"],
        )?;
        let max_inflight = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_max_inflight",
                "Configured max inflight (0 = unset / use default)",
            ),
            &["node"],
        )?;
        let reserved_vram = GaugeVec::new(
            Opts::new(
                "ollama_router_node_reserved_vram_gb",
                "Reservation-ledger VRAM (GiB)",
            ),
            &["node"],
        )?;
        let loaded_vram = GaugeVec::new(
            Opts::new(
                "ollama_router_node_loaded_vram_gb",
                "Loaded VRAM from /api/ps merge (GiB). Gate on loaded_vram_known.",
            ),
            &["node"],
        )?;
        let loaded_vram_known = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_loaded_vram_known",
                "1 if loaded_vram_gb was measured from /api/ps",
            ),
            &["node"],
        )?;
        let disk_available = GaugeVec::new(
            Opts::new(
                "ollama_router_node_disk_available_gb",
                "Models-dir filesystem available (GiB)",
            ),
            &["node"],
        )?;
        let ollama_up = IntGaugeVec::new(
            Opts::new(
                "ollama_router_node_ollama_up",
                "1 if the agent reported local Ollama running",
            ),
            &["node"],
        )?;

        registry.register(Box::new(requests.clone()))?;
        registry.register(Box::new(duration.clone()))?;
        registry.register(Box::new(inflight.clone()))?;
        registry.register(Box::new(healthy.clone()))?;
        registry.register(Box::new(vram_free.clone()))?;
        registry.register(Box::new(vram_free_known.clone()))?;
        registry.register(Box::new(vram.clone()))?;
        registry.register(Box::new(vram_used.clone()))?;
        registry.register(Box::new(vram_used_known.clone()))?;
        registry.register(Box::new(pressure.clone()))?;
        registry.register(Box::new(node_info.clone()))?;
        registry.register(Box::new(route_reason.clone()))?;
        registry.register(Box::new(probe_duration.clone()))?;
        registry.register(Box::new(verda_instances.clone()))?;
        registry.register(Box::new(verda_spot_price.clone()))?;
        registry.register(Box::new(verda_events.clone()))?;
        registry.register(Box::new(job_operations.clone()))?;
        registry.register(Box::new(auto_pull_wait.clone()))?;
        registry.register(Box::new(aggregated_models.clone()))?;
        registry.register(Box::new(node_models.clone()))?;
        registry.register(Box::new(discovery.clone()))?;
        registry.register(Box::new(ram_available.clone()))?;
        registry.register(Box::new(ram_available_ratio.clone()))?;
        registry.register(Box::new(ram_available_known.clone()))?;
        registry.register(Box::new(ram_total.clone()))?;
        registry.register(Box::new(gpu_util.clone()))?;
        registry.register(Box::new(gpu_util_known.clone()))?;
        registry.register(Box::new(cpu_usage.clone()))?;
        registry.register(Box::new(cpu_usage_known.clone()))?;
        registry.register(Box::new(loaded_models.clone()))?;
        registry.register(Box::new(backend_info.clone()))?;
        registry.register(Box::new(gpu_vram.clone()))?;
        registry.register(Box::new(gpu_vram_free.clone()))?;
        registry.register(Box::new(gpu_vram_free_known.clone()))?;
        registry.register(Box::new(gpu_temp.clone()))?;
        registry.register(Box::new(fail_streak.clone()))?;
        registry.register(Box::new(draining.clone()))?;
        registry.register(Box::new(max_inflight.clone()))?;
        registry.register(Box::new(reserved_vram.clone()))?;
        registry.register(Box::new(loaded_vram.clone()))?;
        registry.register(Box::new(loaded_vram_known.clone()))?;
        registry.register(Box::new(disk_available.clone()))?;
        registry.register(Box::new(ollama_up.clone()))?;

        Ok(Self {
            registry,
            requests,
            duration,
            inflight,
            healthy,
            vram_free,
            vram_free_known,
            vram,
            vram_used,
            vram_used_known,
            pressure,
            node_info,
            route_reason,
            probe_duration,
            verda_instances,
            verda_spot_price,
            verda_events,
            job_operations,
            auto_pull_wait,
            aggregated_models,
            node_models,
            discovery,
            ram_available,
            ram_available_ratio,
            ram_available_known,
            ram_total,
            gpu_util,
            gpu_util_known,
            cpu_usage,
            cpu_usage_known,
            loaded_models,
            backend_info,
            gpu_vram,
            gpu_vram_free,
            gpu_vram_free_known,
            gpu_temp,
            fail_streak,
            draining,
            max_inflight,
            reserved_vram,
            loaded_vram,
            loaded_vram_known,
            disk_available,
            ollama_up,
        })
    }

    /// Drop gone-node gauge labels, then set live snapshot values.
    pub fn refresh_gauges(&self, fleet: &Registry, fleet_state: &FleetState) {
        self.inflight.reset();
        self.healthy.reset();
        self.vram_free.reset();
        self.vram_free_known.reset();
        self.vram.reset();
        self.vram_used.reset();
        self.vram_used_known.reset();
        self.pressure.reset();
        self.node_info.reset();
        self.node_models.reset();
        self.ram_available.reset();
        self.ram_available_ratio.reset();
        self.ram_available_known.reset();
        self.ram_total.reset();
        self.gpu_util.reset();
        self.gpu_util_known.reset();
        self.cpu_usage.reset();
        self.cpu_usage_known.reset();
        self.loaded_models.reset();
        self.backend_info.reset();
        self.gpu_vram.reset();
        self.gpu_vram_free.reset();
        self.gpu_vram_free_known.reset();
        self.gpu_temp.reset();
        self.fail_streak.reset();
        self.draining.reset();
        self.max_inflight.reset();
        self.reserved_vram.reset();
        self.loaded_vram.reset();
        self.loaded_vram_known.reset();
        self.disk_available.reset();
        self.ollama_up.reset();

        let snap = fleet.snapshot();
        let mut verda_n: i64 = 0;
        for node in &snap {
            let id = node.id.as_str();
            let origin = node.origin.as_str();
            let role = node_role(&node.labels, node.origin);
            set_g(&self.inflight, &[id], f64::from(node.inflight));
            set_i(&self.node_models, &[id], node.models.len() as i64);
            set_i(&self.healthy, &[id], i64::from(node.healthy));
            set_g(&self.vram_free, &[id], node.vram_free_gb.unwrap_or(0.0));
            set_i(
                &self.vram_free_known,
                &[id],
                i64::from(node.vram_free_known),
            );
            set_g(&self.vram, &[id], node.vram_gb());
            set_g(&self.vram_used, &[id], node.vram_used_gb.unwrap_or(0.0));
            set_i(
                &self.vram_used_known,
                &[id],
                i64::from(node.vram_used_known),
            );
            set_i(&self.pressure, &[id], pressure_value(node.pressure_level));
            set_g(&self.node_info, &[id, origin, role.as_str()], 1.0);
            set_g(
                &self.ram_available,
                &[id],
                node.ram_available_gb.unwrap_or(0.0),
            );
            set_g(
                &self.ram_available_ratio,
                &[id],
                node.ram_available_ratio.unwrap_or(0.0),
            );
            set_i(
                &self.ram_available_known,
                &[id],
                i64::from(node.ram_available_gb.is_some()),
            );
            set_g(&self.ram_total, &[id], node.ram_gb());
            set_g(&self.gpu_util, &[id], node.gpu_util_pct.unwrap_or(0.0));
            set_i(&self.gpu_util_known, &[id], i64::from(node.gpu_util_known));
            set_g(&self.cpu_usage, &[id], node.cpu_usage_pct.unwrap_or(0.0));
            set_i(
                &self.cpu_usage_known,
                &[id],
                i64::from(node.cpu_usage_pct.is_some()),
            );
            set_i(
                &self.loaded_models,
                &[id],
                i64::from(node.loaded_model_gauge()),
            );
            set_g(&self.backend_info, &[id, node.gpu_backend.as_str()], 1.0);
            set_i(&self.fail_streak, &[id], i64::from(node.fail_streak));
            set_i(&self.draining, &[id], i64::from(node.draining));
            set_i(
                &self.max_inflight,
                &[id],
                i64::from(node.max_inflight.unwrap_or(0)),
            );
            set_g(&self.reserved_vram, &[id], node.reserved_vram_gb);
            set_g(&self.loaded_vram, &[id], node.loaded_vram_gb.unwrap_or(0.0));
            set_i(
                &self.loaded_vram_known,
                &[id],
                i64::from(node.loaded_vram_gb.is_some()),
            );
            if let Some(disk) = node.disk_available_gb {
                set_g(&self.disk_available, &[id], disk);
            }
            if let Some(up) = node.ollama_running {
                set_i(&self.ollama_up, &[id], i64::from(up));
            }
            for gpu in &node.gpus_detail {
                let gpu_id = gpu.index.to_string();
                let labels = [id, gpu_id.as_str()];
                set_g(&self.gpu_vram, &labels, gpu.vram_total_gb);
                set_g(&self.gpu_vram_free, &labels, gpu.vram_free_gb);
                set_i(
                    &self.gpu_vram_free_known,
                    &labels,
                    i64::from(gpu.vram_free_known),
                );
                if let Some(temp) = gpu.temperature_c {
                    set_g(&self.gpu_temp, &labels, temp);
                }
            }
            if node.origin == NodeOrigin::Verda {
                verda_n += 1;
            }
        }
        self.verda_instances.set(verda_n);
        self.aggregated_models
            .set(fleet.aggregated_tags().len() as i64);

        let price_sum = fleet_state
            .list_verda_nodes()
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|(_, entry)| entry.verda_spot_price_per_hour)
            .sum::<f64>();
        self.verda_spot_price.set(price_sum);
    }

    /// Prometheus 0.0.4 text. Caller must [`Self::refresh_gauges`] first.
    pub fn encode_text(&self) -> Result<String, prometheus::Error> {
        TextEncoder::new().encode_to_string(&self.registry.gather())
    }

    pub fn observe_request(&self, class: &str, status: u16, node: &str, elapsed: Duration) {
        let code = status.to_string();
        if let Ok(c) = self
            .requests
            .get_metric_with_label_values(&[class, code.as_str(), node])
        {
            c.inc();
        }
        if let Ok(h) = self.duration.get_metric_with_label_values(&[class]) {
            h.observe(elapsed.as_secs_f64());
        }
    }

    pub fn route_reason(&self, reason: &str) {
        if let Ok(c) = self.route_reason.get_metric_with_label_values(&[reason]) {
            c.inc();
        }
    }

    pub fn observe_auto_pull_wait(&self, outcome: &str) {
        if let Ok(c) = self.auto_pull_wait.get_metric_with_label_values(&[outcome]) {
            c.inc();
        }
    }

    pub fn observe_probe(&self, elapsed: Duration) {
        self.probe_duration.observe(elapsed.as_secs_f64());
    }

    pub fn observe_discovery(&self, endpoint: &str) {
        if let Ok(c) = self.discovery.get_metric_with_label_values(&[endpoint]) {
            c.inc();
        }
    }

    pub fn stats_json(&self, fleet: &Registry, fleet_state: &FleetState) -> Value {
        self.refresh_gauges(fleet, fleet_state);
        let snap = fleet.snapshot();
        let inflight: Value = snap
            .iter()
            .map(|n| (n.id.as_str().to_string(), json!(n.inflight)))
            .collect::<serde_json::Map<String, Value>>()
            .into();
        json!({
            "nodes": snap.len(),
            "healthy": snap.iter().filter(|n| n.healthy).count(),
            "inflight": inflight,
            "verda_instances": self.verda_instances.get(),
            "verda_spot_price_per_hour": self.verda_spot_price.get(),
        })
    }
}

impl JobObserver for Metrics {
    fn job_terminal(&self, kind: JobKind, status: JobStatus) {
        if let Ok(c) = self
            .job_operations
            .get_metric_with_label_values(&[kind.as_str(), status.as_str()])
        {
            c.inc();
        }
    }
}

impl FleetEvents for Metrics {
    fn verda_event(&self, event: &'static str) {
        if let Ok(c) = self.verda_events.get_metric_with_label_values(&[event]) {
            c.inc();
        }
    }
}

fn set_g(vec: &GaugeVec, labels: &[&str], value: f64) {
    if let Ok(g) = vec.get_metric_with_label_values(labels) {
        g.set(value);
    }
}

fn set_i(vec: &IntGaugeVec, labels: &[&str], value: i64) {
    if let Ok(g) = vec.get_metric_with_label_values(labels) {
        g.set(value);
    }
}

fn node_role(labels: &[String], origin: NodeOrigin) -> String {
    for wanted in ["cpu", "gpu", "verda"] {
        if labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(wanted))
        {
            return wanted.to_string();
        }
    }
    origin.as_str().to_string()
}

fn pressure_value(level: PressureLevel) -> i64 {
    match level {
        PressureLevel::Unknown => 0,
        PressureLevel::Ok => 1,
        PressureLevel::Elevated => 2,
        PressureLevel::Critical => 3,
    }
}

/// Prometheus 0.0.4 content type (SPEC).
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";
