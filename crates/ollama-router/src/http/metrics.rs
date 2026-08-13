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
    vram: GaugeVec,
    pressure: IntGaugeVec,
    node_info: GaugeVec,
    route_reason: IntCounterVec,
    probe_duration: Histogram,
    verda_instances: IntGauge,
    verda_spot_price: Gauge,
    verda_events: IntCounterVec,
    job_operations: IntCounterVec,
    auto_pull_wait: IntCounterVec,
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
                "Reported free VRAM headroom per node (GiB)",
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

        registry.register(Box::new(requests.clone()))?;
        registry.register(Box::new(duration.clone()))?;
        registry.register(Box::new(inflight.clone()))?;
        registry.register(Box::new(healthy.clone()))?;
        registry.register(Box::new(vram_free.clone()))?;
        registry.register(Box::new(vram.clone()))?;
        registry.register(Box::new(pressure.clone()))?;
        registry.register(Box::new(node_info.clone()))?;
        registry.register(Box::new(route_reason.clone()))?;
        registry.register(Box::new(probe_duration.clone()))?;
        registry.register(Box::new(verda_instances.clone()))?;
        registry.register(Box::new(verda_spot_price.clone()))?;
        registry.register(Box::new(verda_events.clone()))?;
        registry.register(Box::new(job_operations.clone()))?;
        registry.register(Box::new(auto_pull_wait.clone()))?;

        Ok(Self {
            registry,
            requests,
            duration,
            inflight,
            healthy,
            vram_free,
            vram,
            pressure,
            node_info,
            route_reason,
            probe_duration,
            verda_instances,
            verda_spot_price,
            verda_events,
            job_operations,
            auto_pull_wait,
        })
    }

    /// Drop gone-node gauge labels, then set live snapshot values.
    pub fn refresh_gauges(&self, fleet: &Registry, fleet_state: &FleetState) {
        self.inflight.reset();
        self.healthy.reset();
        self.vram_free.reset();
        self.vram.reset();
        self.pressure.reset();
        self.node_info.reset();

        let snap = fleet.snapshot();
        let mut verda_n: i64 = 0;
        for node in &snap {
            let id = node.id.as_str();
            let origin = node.origin.as_str();
            let role = node_role(&node.labels, node.origin);
            if let Ok(g) = self.inflight.get_metric_with_label_values(&[id]) {
                g.set(f64::from(node.inflight));
            }
            if let Ok(g) = self.healthy.get_metric_with_label_values(&[id]) {
                g.set(i64::from(node.healthy));
            }
            if let Ok(g) = self.vram_free.get_metric_with_label_values(&[id]) {
                g.set(node.vram_free_gb.unwrap_or(0.0));
            }
            if let Ok(g) = self.vram.get_metric_with_label_values(&[id]) {
                g.set(node.vram_gb());
            }
            if let Ok(g) = self.pressure.get_metric_with_label_values(&[id]) {
                g.set(pressure_value(node.pressure_level));
            }
            if let Ok(g) = self
                .node_info
                .get_metric_with_label_values(&[id, origin, role.as_str()])
            {
                g.set(1.0);
            }
            if node.origin == NodeOrigin::Verda {
                verda_n += 1;
            }
        }
        self.verda_instances.set(verda_n);

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
