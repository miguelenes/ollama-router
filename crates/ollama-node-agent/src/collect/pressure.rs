//! Agent-owned pressure_level (worst-wins). Router trusts the wire token.

use ollama_capacity_types::{bytes_to_gib, Pressure};
use sysinfo::System;

const RAM_ELEVATED_AVAILABLE_RATIO: f64 = 0.25;
const RAM_CRITICAL_AVAILABLE_RATIO: f64 = 0.12;
const RAM_ELEVATED_AVAILABLE_GB: f64 = 4.0;
const RAM_CRITICAL_AVAILABLE_GB: f64 = 2.0;
const RAM_SWAP_ELEVATED_GB: f64 = 0.5;
const RAM_LOAD_ELEVATED_PER_CORE: f64 = 1.5;
const RAM_LOAD_CRITICAL_PER_CORE: f64 = 3.0;

pub fn from_sysinfo(sys: &System) -> Pressure {
    let total_bytes = sys.total_memory();
    let available_bytes = sys.available_memory();
    let total_gb = bytes_to_gib(total_bytes);
    let available_gb = if total_bytes > 0 {
        Some(bytes_to_gib(available_bytes).max(0.0))
    } else {
        None
    };
    let available_ratio = if total_bytes > 0 {
        Some((available_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0))
    } else {
        None
    };
    let used_ratio = available_ratio.map(|r| (1.0 - r).clamp(0.0, 1.0));
    let swap_total_gb = bytes_to_gib(sys.total_swap());
    let swap_used_gb = if swap_total_gb > 0.0 {
        Some(bytes_to_gib(sys.used_swap()).max(0.0))
    } else {
        None
    };
    let cpu_cores = sys.cpus().len() as i32;
    let load = System::load_average();
    let load1 = Some(load.one.max(0.0));
    let load_per_cpu = if cpu_cores > 0 {
        Some(load.one.max(0.0) / f64::from(cpu_cores))
    } else {
        None
    };
    let source = if total_bytes > 0 {
        Some("MemAvailable".into())
    } else {
        None
    };
    Pressure {
        ram_total_gb: (total_bytes > 0).then_some(total_gb),
        ram_available_gb: available_gb,
        ram_available_ratio: available_ratio,
        ram_used_ratio: used_ratio,
        swap_used_gb,
        swap_total_gb: Some(swap_total_gb),
        load1,
        load5: Some(load.five.max(0.0)),
        load15: Some(load.fifteen.max(0.0)),
        cpu_cores: Some(cpu_cores),
        collected_at: epoch_now(),
        load_1m: load1,
        load_per_cpu,
        ram_available_source: source,
    }
}

fn epoch_now() -> Option<f64> {
    let ns = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    Some(ns as f64 / 1_000_000_000.0)
}

pub fn classify_pressure(p: &Pressure) -> &'static str {
    let has_signal = p.ram_available_gb.is_some() || p.load1.is_some() || p.load_1m.is_some();
    if !has_signal {
        return "unknown";
    }
    let mut level = "ok";
    let available_ratio =
        p.ram_available_ratio
            .or_else(|| match (p.ram_total_gb, p.ram_available_gb) {
                (Some(total), Some(available)) if total > 0.0 => {
                    Some((available / total).clamp(0.0, 1.0))
                }
                _ => None,
            });
    if let Some(ratio) = available_ratio {
        if ratio < RAM_CRITICAL_AVAILABLE_RATIO {
            level = "critical";
        } else if ratio < RAM_ELEVATED_AVAILABLE_RATIO {
            level = "elevated";
        }
    }
    if let Some(available) = p.ram_available_gb {
        if available < RAM_CRITICAL_AVAILABLE_GB {
            level = "critical";
        } else if available < RAM_ELEVATED_AVAILABLE_GB && level != "critical" {
            level = "elevated";
        }
    }
    if let Some(swap) = p.swap_used_gb {
        if swap >= RAM_SWAP_ELEVATED_GB && level == "ok" {
            level = "elevated";
        }
    }
    let load1 = p.load1.or(p.load_1m);
    let per_core = match (p.cpu_cores, load1) {
        (Some(cores), Some(load)) if cores > 0 => Some(load / f64::from(cores)),
        (None | Some(0), _) => p.load_per_cpu,
        _ => None,
    };
    if let Some(per_core) = per_core {
        if per_core >= RAM_LOAD_CRITICAL_PER_CORE {
            level = "critical";
        } else if per_core >= RAM_LOAD_ELEVATED_PER_CORE && level == "ok" {
            level = "elevated";
        }
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_on_low_available() {
        let p = Pressure {
            ram_available_gb: Some(1.0),
            ram_available_ratio: Some(0.05),
            load1: Some(0.1),
            cpu_cores: Some(8),
            ..Default::default()
        };
        assert_eq!(classify_pressure(&p), "critical");
    }

    #[test]
    fn elevated_on_swap() {
        let p = Pressure {
            ram_available_gb: Some(16.0),
            ram_available_ratio: Some(0.5),
            swap_used_gb: Some(2.0),
            load1: Some(0.1),
            cpu_cores: Some(8),
            ..Default::default()
        };
        assert_eq!(classify_pressure(&p), "elevated");
    }

    #[test]
    fn unknown_without_signal() {
        assert_eq!(classify_pressure(&Pressure::default()), "unknown");
    }
}
