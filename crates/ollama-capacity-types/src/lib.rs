//! Wire DTOs for `GET /v1/capacity` and `GET /v1/pressure`.
//!
//! Serde ignores unknown fields (the agent may add columns). Do not use
//! `deny_unknown_fields`. JSON keys match Rust field names (no serde rename).

use serde::{Deserialize, Serialize};

/// Bytes in one gibibyte. sysinfo reports RAM in **bytes**.
pub const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Convert a byte count to GiB (`bytes / 1024³`). Never divide by `1024²`.
pub fn bytes_to_gib(bytes: u64) -> f64 {
    bytes as f64 / BYTES_PER_GIB
}

/// MiB → GiB for nvidia-smi `nounits` memory columns.
pub fn mib_to_gib(mib: f64) -> f64 {
    mib / 1024.0
}

/// GPU backend reported on `/v1/status` and optionally `/v1/capacity`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuBackend {
    #[default]
    Unknown,
    Cpu,
    Cuda,
    Rocm,
    Metal,
}

impl GpuBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Rocm => "rocm",
            Self::Metal => "metal",
        }
    }
}

/// Nested pressure object on capacity / pressure envelopes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pressure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_total_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_available_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_available_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_used_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_used_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_total_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load5: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load15: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_1m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_per_cpu: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_available_source: Option<String>,
}

/// Per-GPU row from nvidia-smi / rocm-smi.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization_gpu_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization_memory_pct: Option<f64>,
}

/// `GET /v1/capacity` body. Nested `pressure` is optional; there is no
/// top-level `pressure_level` on this document.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_memory_limit_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    /// Additive. Router ignores unknown / extra keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_backend: Option<GpuBackend>,
}

/// `GET /v1/pressure` envelope: `collected_at` + `pressure` + `pressure_level` + `live`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PressureEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collected_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<Pressure>,
}

impl PressureEnvelope {
    /// Nested pressure object (`pressure`, else `live`).
    pub fn pressure(&self) -> Option<&Pressure> {
        self.pressure.as_ref().or(self.live.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_to_gib_uses_1024_cubed() {
        assert!((bytes_to_gib(32 * 1024 * 1024 * 1024) - 32.0).abs() < 1e-9);
        assert!((bytes_to_gib(1024 * 1024) - (1.0 / 1024.0)).abs() < 1e-12);
    }

    #[test]
    fn extra_json_fields_are_ignored() {
        let raw = r#"{"vram_gb":0,"gpus":0,"ram_gb":8,"cpu_cores":4,"hostname":"cpu","collected_at":"t","gpu_names":[],"agent_version":"0","gpus_detail":[],"vram_used_gb":0,"vram_free_gb":0,"future_column":true,"gpu_backend":"cpu"}"#;
        let report: CapacityReport = serde_json::from_str(raw).expect("extras");
        assert_eq!(report.gpus, 0);
        assert_eq!(report.gpu_backend, Some(GpuBackend::Cpu));
    }
}
