//! Live inventory: sysinfo RAM/CPU, GPU probes, Ollama liveness, pressure.

mod cpu;
mod disk;
mod metal;
mod nvidia;
mod pressure;
mod psi;
mod ram;
mod rocm;
mod windows_gpu;

use std::path::Path;
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
pub use psi::parse_psi_some_avg10;
pub use ram::ram_available_source;
pub use rocm::parse_rocm_csv;

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
    run_nvidia(&[
        "--query-gpu=name,memory.total,memory.used,memory.free,utilization.gpu,utilization.memory,temperature.gpu,power.draw,clocks.sm,fan.speed,pstate",
        "--format=csv,noheader,nounits",
    ])
    .await
}

pub async fn probe_nvidia_basic() -> Option<String> {
    run_nvidia(&[
        "--query-gpu=name,memory.total",
        "--format=csv,noheader,nounits",
    ])
    .await
}

pub async fn probe_rocm() -> Option<String> {
    run_program("rocm-smi", &["--showmeminfo", "vram", "--csv"]).await
}

async fn run_nvidia(args: &[&str]) -> Option<String> {
    for program in nvidia::nvidia_smi_candidates() {
        if program != Path::new("nvidia-smi") && !program.is_file() {
            continue;
        }
        if let Some(out) = run_program(&program, args).await {
            return Some(out);
        }
    }
    None
}

async fn run_program(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<String> {
    let fut = Command::new(program).args(args).output();
    match timeout(COLLECT_TIMEOUT, fut).await {
        Ok(Ok(out)) if out.status.success() => String::from_utf8(out.stdout).ok(),
        _ => None,
    }
}

async fn probe_metal_displays() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    run_program("system_profiler", &["SPDisplaysDataType", "-json"]).await
}

async fn probe_windows_adapters() -> Option<String> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    run_program(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
        ],
    )
    .await
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
    ollama_tags_probe(base).await.ok
}

/// Result of probing local Ollama `GET /api/tags`. Count only — never store names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TagsProbe {
    pub ok: bool,
    pub model_count: u64,
}

/// Length of the `models` array in an `/api/tags` body. Does not keep names.
pub fn tags_model_count(body: &[u8]) -> u64 {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return 0;
    };
    value
        .get("models")
        .and_then(|models| models.as_array())
        .map(|models| models.len() as u64)
        .unwrap_or(0)
}

pub async fn ollama_tags_probe(base: &str) -> TagsProbe {
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .timeout(COLLECT_TIMEOUT)
        .use_rustls_tls()
        .build()
    else {
        return TagsProbe::default();
    };
    let Ok(resp) = client.get(url).send().await else {
        return TagsProbe::default();
    };
    if !resp.status().is_success() {
        return TagsProbe::default();
    }
    match resp.bytes().await {
        Ok(bytes) => TagsProbe {
            ok: true,
            model_count: tags_model_count(&bytes),
        },
        Err(_) => TagsProbe {
            ok: true,
            model_count: 0,
        },
    }
}

/// `/api/ps` loaded count + VRAM sum. Names are status-only.
#[derive(Clone, Debug, Default)]
pub struct PsProbe {
    pub count: i32,
    pub vram_gb: Option<f64>,
    pub names: Vec<String>,
}

/// Parse Ollama `GET /api/ps` JSON. Does not feed Prometheus labels.
pub fn parse_ps_body(body: &[u8]) -> PsProbe {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return PsProbe::default();
    };
    let Some(models) = value.get("models").and_then(|m| m.as_array()) else {
        return PsProbe::default();
    };
    let mut names = Vec::new();
    let mut vram_bytes: u64 = 0;
    let mut any_vram = false;
    for model in models {
        if let Some(name) = model.get("name").and_then(|n| n.as_str()) {
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
        if let Some(size) = model
            .get("size_vram")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                model
                    .get("size_vram")
                    .and_then(serde_json::Value::as_f64)
                    .map(|v| v as u64)
            })
        {
            any_vram = true;
            vram_bytes = vram_bytes.saturating_add(size);
        }
    }
    PsProbe {
        count: models.len() as i32,
        vram_gb: any_vram.then_some(bytes_to_gib(vram_bytes)),
        names,
    }
}

async fn ollama_ps_probe(base: &str) -> PsProbe {
    let url = format!("{}/api/ps", base.trim_end_matches('/'));
    let Ok(client) = reqwest::Client::builder()
        .timeout(COLLECT_TIMEOUT)
        .use_rustls_tls()
        .build()
    else {
        return PsProbe::default();
    };
    let Ok(resp) = client.get(url).send().await else {
        return PsProbe::default();
    };
    if !resp.status().is_success() {
        return PsProbe::default();
    }
    match resp.bytes().await {
        Ok(bytes) => parse_ps_body(&bytes),
        Err(_) => PsProbe::default(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusPayload {
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub ollama_version: Option<String>,
    pub ollama_listen: String,
    pub gpu_backend: GpuBackend,
    pub models_dir: Option<String>,
    pub models_on_disk: u64,
    pub agent_version: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metal_recommended_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_memory_gb: Option<f64>,
    pub nvidia_smi_ok: bool,
    pub rocm_smi_ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collect_warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaded_model_names: Vec<String>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub report: CapacityReport,
    pub envelope: PressureEnvelope,
    pub status: StatusPayload,
    pub collect_seconds: f64,
}

/// Local Ollama liveness inputs for [`collect_from_parts`].
#[derive(Clone, Debug, Default)]
pub struct OllamaPresence {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    pub models_on_disk: u64,
}

/// Extra live signals bundled to keep [`collect_from_parts`] under clippy's arg limit.
#[derive(Clone, Debug, Default)]
pub struct CollectParts {
    pub gpu: GpuInventory,
    pub backend: GpuBackend,
    pub ollama_listen: String,
    pub ollama: OllamaPresence,
    pub cpu_usage_pct: Option<f64>,
    pub disk_total_gb: Option<f64>,
    pub disk_available_gb: Option<f64>,
    pub loaded_model_count: Option<i32>,
    pub loaded_vram_gb: Option<f64>,
    pub nvidia_smi_ok: bool,
    pub rocm_smi_ok: bool,
    pub collect_warnings: Vec<String>,
    pub loaded_names: Vec<String>,
}

/// Collect using already-fetched GPU CSV (tests + live).
pub fn collect_from_parts(cfg: &AgentConfig, parts: CollectParts) -> Snapshot {
    let started = Instant::now();
    let models_dir = cfg
        .ollama
        .models_dir
        .clone()
        .or_else(|| Some(disk::default_models_dir().display().to_string()));
    let (disk_total_gb, disk_available_gb) = match (parts.disk_total_gb, parts.disk_available_gb) {
        (Some(total), Some(avail)) => (Some(total), Some(avail)),
        _ => match disk::models_dir_disk(cfg.ollama.models_dir.as_deref()) {
            Some((total, avail)) => (Some(total), Some(avail)),
            None => (parts.disk_total_gb, parts.disk_available_gb),
        },
    };
    let (pressure, ram_gb, cpu_cores, hostname, cgroup, metal_gb) =
        sysinfo_snapshot(parts.backend, parts.cpu_usage_pct);
    let mut pressure = pressure;
    if parts.cpu_usage_pct.is_some() {
        pressure.cpu_usage_pct = parts.cpu_usage_pct;
    }
    let level = classify_pressure(&pressure);
    let now = rfc3339_now();
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let unified = (parts.backend == GpuBackend::Metal && ram_gb > 0.0).then_some(ram_gb);
    let report = CapacityReport {
        vram_gb: parts.gpu.vram_gb,
        gpus: parts.gpu.gpus,
        ram_gb,
        cpu_cores,
        hostname: hostname.clone(),
        collected_at: now.clone(),
        gpu_names: parts.gpu.gpu_names.clone(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        gpus_detail: parts.gpu.gpus_detail.clone(),
        vram_used_gb: parts.gpu.vram_used_gb,
        vram_free_gb: parts.gpu.vram_free_gb,
        cgroup_memory_limit_gb: cgroup,
        pressure: Some(pressure.clone()),
        gpu_backend: Some(parts.backend),
        vram_free_known: Some(parts.gpu.vram_free_known),
        vram_used_known: Some(parts.gpu.vram_used_known),
        os: Some(os.clone()),
        arch: Some(arch.clone()),
        cpu_usage_pct: parts.cpu_usage_pct,
        disk_total_gb,
        disk_available_gb,
        loaded_model_count: parts.loaded_model_count,
        loaded_vram_gb: parts.loaded_vram_gb,
        ollama_running: Some(parts.ollama.running),
    };
    let envelope = PressureEnvelope {
        collected_at: Some(now),
        pressure: Some(pressure.clone()),
        pressure_level: Some(level.to_string()),
        live: Some(pressure),
    };
    let status = StatusPayload {
        ollama_installed: parts.ollama.installed,
        ollama_running: parts.ollama.running,
        ollama_version: parts.ollama.version,
        ollama_listen: parts.ollama_listen,
        gpu_backend: parts.backend,
        models_dir,
        models_on_disk: parts.ollama.models_on_disk,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        os,
        arch,
        hostname,
        metal_recommended_gb: metal_gb,
        unified_memory_gb: unified,
        nvidia_smi_ok: parts.nvidia_smi_ok,
        rocm_smi_ok: parts.rocm_smi_ok,
        collect_warnings: parts.collect_warnings,
        loaded_model_names: parts.loaded_names,
    };
    Snapshot {
        report,
        envelope,
        status,
        collect_seconds: started.elapsed().as_secs_f64(),
    }
}

fn sysinfo_snapshot(
    backend: GpuBackend,
    cpu_usage_pct: Option<f64>,
) -> (Pressure, f64, i32, String, Option<f64>, Option<f64>) {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let hostname = System::host_name().unwrap_or_default();
    let ram_gb = bytes_to_gib(sys.total_memory());
    let cpu_cores = sys.cpus().len() as i32;
    let pressure = pressure::from_sysinfo(&sys, cpu_usage_pct);
    let cgroup = sys
        .cgroup_limits()
        .map(|l| bytes_to_gib(cgroup_total_bytes(&l)))
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
            if nvidia {
                GpuBackend::Cuda
            } else if cfg!(target_os = "macos") {
                GpuBackend::Metal
            } else if rocm {
                GpuBackend::Rocm
            } else {
                GpuBackend::Cpu
            }
        }
    }
}

pub fn gpu_from_probes(
    policy: GpuPolicy,
    nvidia_rich: Option<&str>,
    nvidia_basic: Option<&str>,
    rocm: Option<&str>,
) -> (GpuInventory, GpuBackend) {
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
                (rocm.and_then(parse_rocm_csv).unwrap_or_default(), backend)
            } else if backend == GpuBackend::Metal {
                (GpuInventory::default(), backend)
            } else {
                (GpuInventory::default(), GpuBackend::Cpu)
            }
        }
    }
}

/// Full live collect (sysinfo on a blocking thread).
pub async fn collect_live(
    cfg: &AgentConfig,
    ollama_listen: &str,
    cpu_usage_pct: Option<f64>,
) -> Snapshot {
    let policy = cfg.gpu.policy;
    let skip_gpu = matches!(policy, GpuPolicy::Cpu | GpuPolicy::Metal);
    let (nvidia_rich, nvidia_basic, rocm, version, metal_json, win_names) = tokio::join!(
        async {
            if skip_gpu {
                None
            } else {
                probe_nvidia_rich().await
            }
        },
        async {
            if skip_gpu {
                None
            } else {
                probe_nvidia_basic().await
            }
        },
        async {
            if skip_gpu {
                None
            } else {
                probe_rocm().await
            }
        },
        ollama_version(),
        probe_metal_displays(),
        probe_windows_adapters(),
    );
    let nvidia_smi_ok = nvidia_rich.is_some() || nvidia_basic.is_some();
    let rocm_smi_ok = rocm.is_some();
    let (mut inv, backend) = gpu_from_probes(
        policy,
        nvidia_rich.as_deref(),
        nvidia_basic.as_deref(),
        rocm.as_deref(),
    );
    let mut warnings = Vec::new();
    if backend == GpuBackend::Metal && inv.gpus == 0 {
        if let Some(metal) = metal_json
            .as_deref()
            .and_then(metal::parse_metal_displays_json)
        {
            inv = metal;
        }
    }
    if inv.gpus == 0 && backend == GpuBackend::Cpu {
        if let Some(raw) = win_names.as_deref() {
            let names = windows_gpu::parse_video_controller_names(raw);
            if !names.is_empty() {
                inv.gpu_names = names;
            }
        }
    }
    if backend == GpuBackend::Rocm && inv.gpus == 0 && rocm_smi_ok {
        warnings.push("rocm_smi_unparsed".into());
    }
    let installed = version.is_some();
    let ollama_base = format!("http://{ollama_listen}");
    let probe = if installed {
        ollama_tags_probe(&ollama_base).await
    } else {
        TagsProbe::default()
    };
    let ps = if probe.ok {
        ollama_ps_probe(&ollama_base).await
    } else {
        PsProbe::default()
    };
    let running = probe.ok;
    let models_on_disk = probe.model_count;
    let cfg = cfg.clone();
    let ollama_listen = ollama_listen.to_string();
    let parts = CollectParts {
        gpu: inv,
        backend,
        ollama_listen: ollama_listen.clone(),
        ollama: OllamaPresence {
            installed,
            running,
            version,
            models_on_disk,
        },
        cpu_usage_pct,
        loaded_model_count: (ps.count > 0 || running).then_some(ps.count),
        loaded_vram_gb: ps.vram_gb,
        nvidia_smi_ok,
        rocm_smi_ok,
        collect_warnings: warnings,
        loaded_names: ps.names,
        ..CollectParts::default()
    };
    tokio::task::spawn_blocking(move || collect_from_parts(&cfg, parts))
        .await
        .unwrap_or_else(|_| {
            collect_from_parts(
                &AgentConfig::default(),
                CollectParts {
                    backend: GpuBackend::Unknown,
                    ollama_listen: "127.0.0.1:11434".into(),
                    ..CollectParts::default()
                },
            )
        })
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
        assert!(!inv.vram_free_known);
    }

    #[test]
    fn gpu_probe_trait_feeds_parser() {
        struct MockNvidia(&'static str);
        impl GpuProbe for MockNvidia {
            fn nvidia_rich(&self) -> Option<String> {
                Some(self.0.into())
            }
            fn nvidia_basic(&self) -> Option<String> {
                None
            }
            fn rocm(&self) -> Option<String> {
                None
            }
        }
        let probe = MockNvidia("NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28\n");
        let inv = parse_nvidia_csv(&probe.nvidia_rich().unwrap()).unwrap();
        assert!((inv.vram_gb - 8.0).abs() < 1e-9);
        assert_eq!(inv.gpus, 1);
    }

    #[test]
    fn auto_nvidia_sets_cuda() {
        let csv = "NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28\n";
        let (inv, backend) = gpu_from_probes(GpuPolicy::Auto, Some(csv), None, None);
        assert_eq!(backend, GpuBackend::Cuda);
        assert_eq!(inv.gpus, 1);
        assert!((inv.vram_gb - 8.0).abs() < 1e-9);
        assert!(inv.vram_free_known);
    }

    #[test]
    fn metal_collect_sets_unknown_vram() {
        let snap = collect_from_parts(
            &AgentConfig::default(),
            CollectParts {
                backend: GpuBackend::Metal,
                ollama_listen: "127.0.0.1:11434".into(),
                ..CollectParts::default()
            },
        );
        assert_eq!(snap.report.gpu_backend, Some(GpuBackend::Metal));
        assert_eq!(snap.report.vram_free_known, Some(false));
        assert!(!snap.report.vram_free_is_known());
        if snap.report.ram_gb > 0.0 {
            assert!(snap.status.metal_recommended_gb.is_some());
        }
    }

    #[test]
    fn tags_model_count_empty() {
        assert_eq!(tags_model_count(br#"{"models":[]}"#), 0);
        assert_eq!(tags_model_count(br#"{}"#), 0);
        assert_eq!(tags_model_count(b"not-json"), 0);
    }

    #[test]
    fn tags_model_count_one() {
        assert_eq!(tags_model_count(br#"{"models":[{"name":"x"}]}"#), 1);
    }

    #[test]
    fn tags_model_count_dup_names_are_array_length() {
        assert_eq!(
            tags_model_count(br#"{"models":[{"name":"a"},{"name":"a"}]}"#),
            2
        );
    }

    #[test]
    fn parse_ps_sums_vram_without_requiring_names_on_metrics() {
        let ps = parse_ps_body(
            br#"{"models":[{"name":"a","size_vram":1073741824},{"name":"b","size_vram":1073741824}]}"#,
        );
        assert_eq!(ps.count, 2);
        assert!((ps.vram_gb.unwrap() - 2.0).abs() < 1e-6);
        assert_eq!(ps.names.len(), 2);
    }
}
