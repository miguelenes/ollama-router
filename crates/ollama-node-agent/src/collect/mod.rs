//! Live inventory: sysinfo RAM/CPU, GPU probes, Ollama liveness, pressure.

mod nvidia;
mod pressure;

use std::time::{Duration, Instant};

use ollama_capacity_types::{
    bytes_to_gib, CapacityReport, GpuBackend, GpuDetail, Pressure, PressureEnvelope,
};
use serde::Serialize;
use sysinfo::System;
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::{AgentConfig, GpuPolicy};

pub use nvidia::{parse_nvidia_csv, parse_nvidia_fallback_csv, GpuInventory};
pub use pressure::classify_pressure;

const COLLECT_TIMEOUT: Duration = Duration::from_secs(2);

/// GPU subprocess output (injected in tests).
pub trait GpuProbe: Send + Sync {
    fn nvidia_rich(&self) -> Option<String>;
    fn nvidia_basic(&self) -> Option<String>;
    fn rocm(&self) -> Option<String>;
}

pub struct LiveGpuProbe;

impl GpuProbe for LiveGpuProbe {
    fn nvidia_rich(&self) -> Option<String> {
        None
    }
    fn nvidia_basic(&self) -> Option<String> {
        None
    }
    fn rocm(&self) -> Option<String> {
        None
    }
}

/// Async live probe using `tokio::process::Command` + timeout.
pub async fn probe_nvidia_rich() -> Option<String> {
    run_csv(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,memory.used,memory.free,utilization.gpu,utilization.memory",
            "--format=csv,noheader,nounits",
        ],
    )
    .await
}

pub async fn probe_nvidia_basic() -> Option<String> {
    run_csv(
        "nvidia-smi",
        &["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"],
    )
    .await
}

pub async fn probe_rocm() -> Option<String> {
    run_csv("rocm-smi", &["--showmeminfo", "vram", "--csv"]).await
}

async fn run_csv(program: &str, args: &[&str]) -> Option<String> {
    let fut = Command::new(program).args(args).output();
    match timeout(COLLECT_TIMEOUT, fut).await {
        Ok(Ok(out)) if out.status.success() => String::from_utf8(out.stdout).ok(),
        _ => None,
    }
}

pub async fn ollama_version() -> Option<String> {
    let fut = Command::new("ollama").arg("--version").output();
    match timeout(COLLECT_TIMEOUT, fut).await {
        Ok(Ok(out)) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.lines().next().unwrap_or(t).trim().to_string())
            }
        }
        _ => None,
    }
}

pub async fn ollama_tags_ok(base: &str) -> bool {
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .timeout(COLLECT_TIMEOUT)
        .use_rustls_tls()
        .build()
    else {
        return false;
    };
    client.get(url).send().await.is_ok_and(|r| r.status().is_success())
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusPayload {
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub ollama_version: Option<String>,
    pub ollama_listen: String,
    pub gpu_backend: GpuBackend,
    pub models_dir: Option<String>,
    pub agent_version: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metal_recommended_gb: Option<f64>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub report: CapacityReport,
    pub envelope: PressureEnvelope,
    pub status: StatusPayload,
    pub collect_seconds: f64,
}

/// Collect using already-fetched GPU CSV (tests + live).
pub fn collect_from_parts(
    cfg: &AgentConfig,
    gpu: GpuInventory,
    backend: GpuBackend,
    ollama_listen: &str,
    ollama_installed: bool,
    ollama_running: bool,
    ollama_version: Option<String>,
) -> Snapshot {
    let started = Instant::now();
    let (pressure, ram_gb, cpu_cores, hostname, cgroup, metal_gb) = sysinfo_snapshot(backend);
    let level = classify_pressure(&pressure);
    let now = rfc3339_now();
    let report = CapacityReport {
        vram_gb: gpu.vram_gb,
        gpus: gpu.gpus,
        ram_gb,
        cpu_cores,
        hostname: hostname.clone(),
        collected_at: now.clone(),
        gpu_names: gpu.gpu_names.clone(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        gpus_detail: gpu.gpus_detail.clone(),
        vram_used_gb: gpu.vram_used_gb,
        vram_free_gb: gpu.vram_free_gb,
        cgroup_memory_limit_gb: cgroup,
        pressure: Some(pressure.clone()),
        gpu_backend: Some(backend),
    };
    let envelope = PressureEnvelope {
        collected_at: Some(now),
        pressure: Some(pressure.clone()),
        pressure_level: Some(level.to_string()),
        live: Some(pressure),
    };
    let status = StatusPayload {
        ollama_installed,
        ollama_running,
        ollama_version,
        ollama_listen: ollama_listen.to_string(),
        gpu_backend: backend,
        models_dir: cfg.ollama.models_dir.clone(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        hostname,
        metal_recommended_gb: metal_gb,
    };
    Snapshot {
        report,
        envelope,
        status,
        collect_seconds: started.elapsed().as_secs_f64(),
    }
}

fn sysinfo_snapshot(backend: GpuBackend) -> (Pressure, f64, i32, String, Option<f64>, Option<f64>) {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let hostname = System::host_name().unwrap_or_default();
    let ram_gb = bytes_to_gib(sys.total_memory());
    let cpu_cores = sys.cpus().len() as i32;
    let pressure = pressure::from_sysinfo(&sys);
    let cgroup = sys.cgroup_limits().map(|l| bytes_to_gib(cgroup_total_bytes(&l)))
        .or_else(read_cgroup_memory_limit_gb);
    let metal_gb = if backend == GpuBackend::Metal && sys.total_memory() > 0 {
        Some(bytes_to_gib(sys.available_memory()))
    } else {
        None
    };
    (pressure, ram_gb, cpu_cores, hostname, cgroup, metal_gb)
}

fn cgroup_total_bytes(limits: &sysinfo::CGroupLimits) -> u64 {
    limits.total_memory
}

fn read_cgroup_memory_limit_gb() -> Option<f64> {
    for path in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let value = raw.trim();
        if value == "max" {
            continue;
        }
        if let Ok(bytes) = value.parse::<u64>() {
            if bytes > 0 {
                return Some(bytes_to_gib(bytes));
            }
        }
    }
    None
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

pub fn select_backend(policy: GpuPolicy, nvidia: bool, rocm: bool) -> GpuBackend {
    match policy {
        GpuPolicy::Cpu => GpuBackend::Cpu,
        GpuPolicy::Cuda => GpuBackend::Cuda,
        GpuPolicy::Rocm => GpuBackend::Rocm,
        GpuPolicy::Metal => GpuBackend::Metal,
        GpuPolicy::Auto => {
            if cfg!(target_os = "macos") {
                GpuBackend::Metal
            } else if nvidia {
                GpuBackend::Cuda
            } else if rocm {
                GpuBackend::Rocm
            } else {
                GpuBackend::Cpu
            }
        }
    }
}

pub fn gpu_from_probes(policy: GpuPolicy, nvidia_rich: Option<&str>, nvidia_basic: Option<&str>, rocm: Option<&str>) -> (GpuInventory, GpuBackend) {
    match policy {
        GpuPolicy::Cpu => (GpuInventory::default(), GpuBackend::Cpu),
        GpuPolicy::Metal => (GpuInventory::default(), GpuBackend::Metal),
        GpuPolicy::Cuda | GpuPolicy::Auto | GpuPolicy::Rocm => {
            let nvidia_inv = nvidia_rich
                .and_then(parse_nvidia_csv)
                .or_else(|| nvidia_basic.and_then(parse_nvidia_fallback_csv))
                .unwrap_or_default();
            let has_nvidia = nvidia_inv.gpus > 0;
            let has_rocm = rocm.is_some();
            let backend = select_backend(policy, has_nvidia, has_rocm);
            if backend == GpuBackend::Cuda {
                (nvidia_inv, backend)
            } else if backend == GpuBackend::Rocm {
                (parse_rocm_placeholder(rocm), backend)
            } else if backend == GpuBackend::Metal {
                (GpuInventory::default(), backend)
            } else {
                (GpuInventory::default(), GpuBackend::Cpu)
            }
        }
    }
}

fn parse_rocm_placeholder(csv: Option<&str>) -> GpuInventory {
    // rocm-smi CSV varies by version; presence without a parse still marks backend.
    let _ = csv;
    GpuInventory::default()
}

/// Full live collect (sysinfo on a blocking thread).
pub async fn collect_live(cfg: &AgentConfig, ollama_listen: &str) -> Snapshot {
    let policy = cfg.gpu.policy;
    let (nvidia_rich, nvidia_basic, rocm, version) = tokio::join!(
        probe_nvidia_rich(),
        probe_nvidia_basic(),
        probe_rocm(),
        ollama_version(),
    );
    let skip_gpu = matches!(policy, GpuPolicy::Cpu | GpuPolicy::Metal);
    let (inv, backend) = if skip_gpu {
        gpu_from_probes(policy, None, None, None)
    } else {
        gpu_from_probes(
            policy,
            nvidia_rich.as_deref(),
            nvidia_basic.as_deref(),
            rocm.as_deref(),
        )
    };
    let installed = version.is_some();
    let ollama_base = format!("http://{ollama_listen}");
    let running = if installed {
        ollama_tags_ok(&ollama_base).await
    } else {
        false
    };
    let cfg = cfg.clone();
    let ollama_listen = ollama_listen.to_string();
    tokio::task::spawn_blocking(move || {
        collect_from_parts(
            &cfg,
            inv,
            backend,
            &ollama_listen,
            installed,
            running,
            version,
        )
    })
    .await
    .unwrap_or_else(|_| collect_from_parts(
        &AgentConfig::default(),
        GpuInventory::default(),
        GpuBackend::Unknown,
        "127.0.0.1:11434",
        false,
        false,
        None,
    ))
}

/// nvidia-smi CSV rows for tests.
pub fn inventory_from_details(details: Vec<GpuDetail>) -> GpuInventory {
    nvidia::inventory_from_details(details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ollama_capacity_types::BYTES_PER_GIB;

    #[test]
    fn gib_fixture_32() {
        assert!((bytes_to_gib(32 * 1024 * 1024 * 1024) - 32.0).abs() < 1e-9);
        assert!(((32.0 * BYTES_PER_GIB) as u64).abs_diff(32 * 1024 * 1024 * 1024) < 2);
    }

    #[test]
    fn cpu_policy_zeroes_gpu() {
        let (inv, backend) = gpu_from_probes(
            GpuPolicy::Cpu,
            Some("NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28\n"),
            None,
            None,
        );
        assert_eq!(backend, GpuBackend::Cpu);
        assert_eq!(inv.gpus, 0);
        assert!((inv.vram_gb - 0.0).abs() < 1e-9);
    }

    #[test]
    fn auto_nvidia_sets_cuda() {
        let csv = "NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28\n";
        let (inv, backend) = gpu_from_probes(GpuPolicy::Auto, Some(csv), None, None);
        if cfg!(target_os = "macos") {
            assert_eq!(backend, GpuBackend::Metal);
            assert_eq!(inv.gpus, 0);
        } else {
            assert_eq!(backend, GpuBackend::Cuda);
            assert_eq!(inv.gpus, 1);
            assert!((inv.vram_gb - 8.0).abs() < 1e-9);
        }
    }
}
