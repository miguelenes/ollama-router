//! Prometheus text for the node agent. No model names.

use prometheus::{Gauge, Histogram, HistogramOpts, IntGauge, Registry, TextEncoder};

pub struct AgentMetrics {
    registry: Registry,
    ollama_up: IntGauge,
    ollama_models: IntGauge,
    vram_gb: Gauge,
    ram_available_gb: Gauge,
    ram_available_known: IntGauge,
    gpu_util: Gauge,
    gpu_util_known: IntGauge,
    vram_free_known: IntGauge,
    collect_seconds: Histogram,
}

impl AgentMetrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let ollama_up = IntGauge::new(
            "ollama_node_agent_ollama_up",
            "1 if GET /api/tags succeeded",
        )?;
        let ollama_models = IntGauge::new(
            "ollama_node_agent_models",
            "On-disk model count from GET /api/tags (no names)",
        )?;
        let vram_gb = Gauge::new("ollama_node_agent_gpu_vram_gb", "Sum of GPU VRAM (GiB)")?;
        let ram_available_gb =
            Gauge::new("ollama_node_agent_ram_available_gb", "Available RAM (GiB)")?;
        let ram_available_known = IntGauge::new(
            "ollama_node_agent_ram_available_known",
            "1 if ram_available_gb was measured",
        )?;
        let gpu_util = Gauge::new(
            "ollama_node_agent_gpu_utilization_pct",
            "Mean GPU utilization percent",
        )?;
        let gpu_util_known = IntGauge::new(
            "ollama_node_agent_gpu_util_known",
            "1 if gpu_utilization_pct was measured",
        )?;
        let vram_free_known = IntGauge::new(
            "ollama_node_agent_vram_free_known",
            "1 if free VRAM was measured (0 can be a full GPU)",
        )?;
        let collect_seconds = Histogram::with_opts(HistogramOpts::new(
            "agent_collect_seconds",
            "Wall time of the last collect",
        ))?;
        registry.register(Box::new(ollama_up.clone()))?;
        registry.register(Box::new(ollama_models.clone()))?;
        registry.register(Box::new(vram_gb.clone()))?;
        registry.register(Box::new(ram_available_gb.clone()))?;
        registry.register(Box::new(ram_available_known.clone()))?;
        registry.register(Box::new(gpu_util.clone()))?;
        registry.register(Box::new(gpu_util_known.clone()))?;
        registry.register(Box::new(vram_free_known.clone()))?;
        registry.register(Box::new(collect_seconds.clone()))?;
        Ok(Self {
            registry,
            ollama_up,
            ollama_models,
            vram_gb,
            ram_available_gb,
            ram_available_known,
            gpu_util,
            gpu_util_known,
            vram_free_known,
            collect_seconds,
        })
    }

    pub fn observe(&self, snap: &crate::collect::Snapshot) {
        self.ollama_up.set(i64::from(snap.status.ollama_running));
        self.ollama_models.set(snap.status.models_on_disk as i64);
        self.vram_gb.set(snap.report.vram_gb);
        let ram = snap
            .report
            .pressure
            .as_ref()
            .and_then(|p| p.ram_available_gb);
        self.ram_available_gb.set(ram.unwrap_or(0.0));
        self.ram_available_known.set(i64::from(ram.is_some()));
        let utils: Vec<f64> = snap
            .report
            .gpus_detail
            .iter()
            .filter_map(|g| g.utilization_gpu_pct)
            .collect();
        let util_known = !utils.is_empty();
        let util = if util_known {
            utils.iter().sum::<f64>() / utils.len() as f64
        } else {
            0.0
        };
        self.gpu_util.set(util);
        self.gpu_util_known.set(i64::from(util_known));
        self.vram_free_known
            .set(i64::from(snap.report.vram_free_is_known()));
        self.collect_seconds.observe(snap.collect_seconds);
    }

    pub fn encode_text(&self) -> Result<String, prometheus::Error> {
        TextEncoder::new().encode_to_string(&self.registry.gather())
    }
}

pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";
