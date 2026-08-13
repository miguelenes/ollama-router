//! Wire DTOs for `GET /v1/capacity` and `GET /v1/pressure`.
//!
//! Serde ignores unknown fields (same rule as Verda). Do not use
//! `deny_unknown_fields` — the agent may add columns.

use serde::Deserialize;

/// Nested pressure object on capacity / pressure envelopes.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Pressure {
    #[serde(default)]
    pub ram_total_gb: Option<f64>,
    #[serde(default)]
    pub ram_available_gb: Option<f64>,
    #[serde(default)]
    pub ram_available_ratio: Option<f64>,
    #[serde(default)]
    pub ram_used_ratio: Option<f64>,
    #[serde(default)]
    pub swap_used_gb: Option<f64>,
    #[serde(default)]
    pub swap_total_gb: Option<f64>,
    #[serde(default)]
    pub load1: Option<f64>,
    #[serde(default)]
    pub load5: Option<f64>,
    #[serde(default)]
    pub load15: Option<f64>,
    #[serde(default)]
    pub cpu_cores: Option<i32>,
    #[serde(default)]
    pub collected_at: Option<f64>,
    #[serde(default)]
    pub load_1m: Option<f64>,
    #[serde(default)]
    pub load_per_cpu: Option<f64>,
    #[serde(default)]
    pub ram_available_source: Option<String>,
}

/// Per-GPU row from the agent.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GpuDetail {
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub vram_total_gb: f64,
    #[serde(default)]
    pub vram_used_gb: f64,
    #[serde(default)]
    pub vram_free_gb: f64,
    #[serde(default)]
    pub utilization_gpu_pct: Option<f64>,
    #[serde(default)]
    pub utilization_memory_pct: Option<f64>,
}

/// `GET /v1/capacity` body. Nested `pressure` is optional; there is no
/// top-level `pressure_level` on this document.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CapacityReport {
    #[serde(default)]
    pub vram_gb: f64,
    #[serde(default)]
    pub gpus: i32,
    #[serde(default)]
    pub ram_gb: f64,
    #[serde(default)]
    pub cpu_cores: i32,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub collected_at: String,
    #[serde(default)]
    pub gpu_names: Vec<String>,
    #[serde(default)]
    pub agent_version: String,
    #[serde(default)]
    pub gpus_detail: Vec<GpuDetail>,
    #[serde(default)]
    pub vram_used_gb: f64,
    #[serde(default)]
    pub vram_free_gb: f64,
    #[serde(default)]
    pub cgroup_memory_limit_gb: Option<f64>,
    #[serde(default)]
    pub pressure: Option<Pressure>,
}

impl CapacityReport {
    /// Inventory fields as a [`crate::config::Capacity`] snapshot.
    pub fn as_capacity(&self) -> crate::config::Capacity {
        crate::config::Capacity {
            vram_gb: Some(self.vram_gb),
            ram_gb: Some(self.ram_gb),
            gpus: Some(self.gpus.max(0) as u32),
            cpu_cores: Some(self.cpu_cores.max(0) as u32),
        }
    }
}

/// `GET /v1/pressure` envelope: `collected_at` + `pressure` + `pressure_level` + `live`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PressureEnvelope {
    #[serde(default)]
    pub collected_at: Option<String>,
    #[serde(default)]
    pub pressure: Option<Pressure>,
    #[serde(default)]
    pub pressure_level: Option<String>,
    #[serde(default)]
    pub live: Option<Pressure>,
}

impl PressureEnvelope {
    /// Nested pressure object (`pressure`, else `live`).
    pub fn pressure(&self) -> Option<&Pressure> {
        self.pressure.as_ref().or(self.live.as_ref())
    }
}
