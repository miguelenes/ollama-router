//! Prometheus text for the node agent. No model names.

use prometheus::{Gauge, Histogram, HistogramOpts, IntGauge, Registry, TextEncoder};

pub struct AgentMetrics {
    registry: Registry,
    ollama_up: IntGauge,
    vram_gb: Gauge,
    ram_available_gb: Gauge,
    gpu_util: Gauge,
    collect_seconds: Histogram,
}

impl AgentMetrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let ollama_up = IntGauge::new("ollama_up", "1 if GET /api/tags succeeded")?;
        let vram_gb = Gauge::new("ollama_gpu_vram_gb", "Sum of GPU VRAM (GiB)")?;
        let ram_available_gb = Gauge::new("ram_available_gb", "Available RAM (GiB)")?;
        let gpu_util = Gauge::new("gpu_utilization_pct", "Mean GPU utilization percent")?;
        let collect_seconds = Histogram::with_opts(HistogramOpts::new(
            "agent_collect_seconds",
            "Wall time of the last collect",
        ))?;
        registry.register(Box::new(ollama_up.clone()))?;
        registry.register(Box::new(vram_gb.clone()))?;
        registry.register(Box::new(ram_available_gb.clone()))?;
        registry.register(Box::new(gpu_util.clone()))?;
        registry.register(Box::new(collect_seconds.clone()))?;
        Ok(Self {
            registry,
            ollama_up,
            vram_gb,
            ram_available_gb,
            gpu_util,
            collect_seconds,
        })
    }

    pub fn observe(&self, snap: &crate::collect::Snapshot) {
        self.ollama_up.set(i64::from(snap.status.ollama_running));
        self.vram_gb.set(snap.report.vram_gb);
        let ram = snap
            .report
            .pressure
            .as_ref()
            .and_then(|p| p.ram_available_gb)
            .unwrap_or(0.0);
        self.ram_available_gb.set(ram);
        let util = if snap.report.gpus_detail.is_empty() {
            0.0
        } else {
            let sum: f64 = snap
                .report
                .gpus_detail
                .iter()
                .filter_map(|g| g.utilization_gpu_pct)
                .sum();
            let n = snap
                .report
                .gpus_detail
                .iter()
                .filter(|g| g.utilization_gpu_pct.is_some())
                .count()
                .max(1) as f64;
            sum / n
        };
        self.gpu_util.set(util);
        self.collect_seconds.observe(snap.collect_seconds);
    }

    pub fn encode_text(&self) -> Result<String, prometheus::Error> {
        TextEncoder::new().encode_to_string(&self.registry.gather())
    }
}

pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";
