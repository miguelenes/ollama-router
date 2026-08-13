//! nvidia-smi CSV parsing. VRAM columns are MiB (`nounits`).

use std::path::PathBuf;

use ollama_capacity_types::{mib_to_gib, GpuDetail};

#[derive(Clone, Debug, Default)]
pub struct GpuInventory {
    pub gpus: i32,
    pub vram_gb: f64,
    pub gpu_names: Vec<String>,
    pub gpus_detail: Vec<GpuDetail>,
    pub vram_used_gb: f64,
    pub vram_free_gb: f64,
    pub vram_free_known: bool,
    pub vram_used_known: bool,
}

pub fn inventory_from_details(details: Vec<GpuDetail>) -> GpuInventory {
    let vram_free_known = !details.is_empty() && details.iter().all(GpuDetail::vram_free_is_known);
    let vram_used_known = !details.is_empty() && details.iter().all(GpuDetail::vram_used_is_known);
    GpuInventory {
        gpus: details.len() as i32,
        vram_gb: details.iter().map(|gpu| gpu.vram_total_gb).sum(),
        gpu_names: details.iter().map(|gpu| gpu.name.clone()).collect(),
        vram_used_gb: details
            .iter()
            .filter(|gpu| gpu.vram_used_is_known())
            .map(|gpu| gpu.vram_used_gb)
            .sum(),
        vram_free_gb: details
            .iter()
            .filter(|gpu| gpu.vram_free_is_known())
            .map(|gpu| gpu.vram_free_gb)
            .sum(),
        gpus_detail: details,
        vram_free_known,
        vram_used_known,
    }
}

/// PATH plus well-known install locations (Windows LocalSystem PATH is thin).
pub fn nvidia_smi_candidates() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("nvidia-smi")];
    #[cfg(windows)]
    {
        out.push(PathBuf::from(r"C:\Windows\System32\nvidia-smi.exe"));
        out.push(PathBuf::from(
            r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe",
        ));
    }
    #[cfg(unix)]
    {
        out.push(PathBuf::from("/usr/bin/nvidia-smi"));
    }
    out
}

/// Rich query: name,total,used,free,util.gpu,util.mem[,temp,power,clock,fan,pstate]
pub fn parse_nvidia_csv(stdout: &str) -> Option<GpuInventory> {
    let details: Vec<GpuDetail> = stdout
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_gpu_detail(index as i32, line))
        .collect();
    if details.is_empty() {
        None
    } else {
        Some(inventory_from_details(details))
    }
}

/// Fallback: name,total only. Total is known; free/used are **not** measured.
pub fn parse_nvidia_fallback_csv(stdout: &str) -> Option<GpuInventory> {
    let details: Vec<GpuDetail> = stdout
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let mut parts = line.splitn(2, ',');
            let name = parts.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let total_mib = parts.next()?.trim().parse::<f64>().ok()?;
            (total_mib >= 0.0).then_some(GpuDetail {
                index: index as i32,
                name,
                vram_total_gb: mib_to_gib(total_mib),
                vram_used_gb: 0.0,
                vram_free_gb: 0.0,
                utilization_gpu_pct: None,
                utilization_memory_pct: None,
                vram_free_known: Some(false),
                vram_used_known: Some(false),
                util_known: Some(false),
                ..GpuDetail::default()
            })
        })
        .collect();
    if details.is_empty() {
        None
    } else {
        Some(inventory_from_details(details))
    }
}

fn parse_gpu_detail(index: i32, line: &str) -> Option<GpuDetail> {
    let values: Vec<&str> = line.split(',').map(str::trim).collect();
    if values.len() < 4 {
        return None;
    }
    let parse_mib = |value: &str| value.parse::<f64>().ok().filter(|v| *v >= 0.0);
    let parse_opt_f64 = |position: usize| {
        values
            .get(position)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|v| v.is_finite())
    };
    let parse_pct = |position: usize| parse_opt_f64(position).filter(|v| (0.0..=100.0).contains(v));
    let total_mib = parse_mib(values[1])?;
    let used_mib = parse_mib(values[2]);
    let free_mib = parse_mib(values[3]);
    let used_known = used_mib.is_some();
    let free_known = free_mib.is_some() || used_known;
    let used = used_mib.unwrap_or(0.0);
    let free = free_mib.unwrap_or((total_mib - used).max(0.0));
    let util = parse_pct(4);
    Some(GpuDetail {
        index,
        name: values[0].to_string(),
        vram_total_gb: mib_to_gib(total_mib),
        vram_used_gb: mib_to_gib(used),
        vram_free_gb: mib_to_gib(free),
        utilization_gpu_pct: util,
        utilization_memory_pct: parse_pct(5),
        temperature_c: parse_opt_f64(6).filter(|v| (-40.0..=150.0).contains(v)),
        power_draw_w: parse_opt_f64(7).filter(|v| (0.0..=2000.0).contains(v)),
        clock_sm_mhz: parse_opt_f64(8).filter(|v| (0.0..=5000.0).contains(v)),
        fan_speed_pct: parse_pct(9),
        vram_free_known: Some(free_known),
        vram_used_known: Some(used_known),
        util_known: Some(util.is_some()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rich_csv() {
        let csv = "NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28\n";
        let inv = parse_nvidia_csv(csv).expect("csv");
        assert_eq!(inv.gpus, 1);
        assert!((inv.vram_gb - 8.0).abs() < 1e-9);
        assert!((inv.gpus_detail[0].utilization_gpu_pct.unwrap() - 14.0).abs() < 1e-9);
        assert!(inv.vram_free_known);
        assert!(inv.vram_used_known);
        assert!(inv.gpus_detail[0].vram_free_is_known());
    }

    #[test]
    fn rich_csv_full_gpu_free_zero_is_known() {
        let csv = "NVIDIA GeForce RTX 3070, 8192, 8192, 0, 99, 100\n";
        let inv = parse_nvidia_csv(csv).expect("csv");
        assert!(inv.vram_free_known);
        assert!((inv.vram_free_gb - 0.0).abs() < 1e-12);
        assert_eq!(inv.gpus_detail[0].vram_free_known, Some(true));
    }

    #[test]
    fn fallback_total_known_free_unknown() {
        let csv = "NVIDIA GeForce RTX 3070, 8192\n";
        let inv = parse_nvidia_fallback_csv(csv).expect("csv");
        assert_eq!(inv.gpus, 1);
        assert!((inv.vram_gb - 8.0).abs() < 1e-9);
        assert!(!inv.vram_free_known);
        assert!(!inv.vram_used_known);
        assert_eq!(inv.gpus_detail[0].vram_free_known, Some(false));
        assert_eq!(inv.gpus_detail[0].vram_used_known, Some(false));
    }

    #[test]
    fn missing_smi_is_none() {
        assert!(parse_nvidia_csv("").is_none());
        assert!(parse_nvidia_csv("NVIDIA\n").is_none());
    }

    #[test]
    fn parses_optional_temp_and_power() {
        let csv = "NVIDIA GeForce RTX 3070, 8192, 2304, 5888, 14, 28, 62, 120.5, 1410, 40\n";
        let inv = parse_nvidia_csv(csv).expect("csv");
        let gpu = &inv.gpus_detail[0];
        assert!((gpu.temperature_c.unwrap() - 62.0).abs() < 1e-9);
        assert!((gpu.power_draw_w.unwrap() - 120.5).abs() < 1e-9);
        assert!((gpu.clock_sm_mhz.unwrap() - 1410.0).abs() < 1e-9);
        assert!((gpu.fan_speed_pct.unwrap() - 40.0).abs() < 1e-9);
    }
}
