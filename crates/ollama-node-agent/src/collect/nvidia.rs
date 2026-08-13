//! nvidia-smi CSV parsing. VRAM columns are MiB (`nounits`).

use ollama_capacity_types::{mib_to_gib, GpuDetail};

#[derive(Clone, Debug, Default)]
pub struct GpuInventory {
    pub gpus: i32,
    pub vram_gb: f64,
    pub gpu_names: Vec<String>,
    pub gpus_detail: Vec<GpuDetail>,
    pub vram_used_gb: f64,
    pub vram_free_gb: f64,
}

pub fn inventory_from_details(details: Vec<GpuDetail>) -> GpuInventory {
    GpuInventory {
        gpus: details.len() as i32,
        vram_gb: details.iter().map(|gpu| gpu.vram_total_gb).sum(),
        gpu_names: details.iter().map(|gpu| gpu.name.clone()).collect(),
        vram_used_gb: details.iter().map(|gpu| gpu.vram_used_gb).sum(),
        vram_free_gb: details.iter().map(|gpu| gpu.vram_free_gb).sum(),
        gpus_detail: details,
    }
}

/// Rich query: name,total,used,free,util.gpu,util.mem
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

/// Fallback: name,total only.
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
    let total_mib = parse_mib(values[1])?;
    let used_mib = parse_mib(values[2]).unwrap_or(0.0);
    let free_mib = parse_mib(values[3]).unwrap_or((total_mib - used_mib).max(0.0));
    let parse_pct = |position: usize| {
        values
            .get(position)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|v| (0.0..=100.0).contains(v))
    };
    Some(GpuDetail {
        index,
        name: values[0].to_string(),
        vram_total_gb: mib_to_gib(total_mib),
        vram_used_gb: mib_to_gib(used_mib),
        vram_free_gb: mib_to_gib(free_mib),
        utilization_gpu_pct: parse_pct(4),
        utilization_memory_pct: parse_pct(5),
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
    }

    #[test]
    fn missing_smi_is_none() {
        assert!(parse_nvidia_csv("").is_none());
        assert!(parse_nvidia_csv("NVIDIA\n").is_none());
    }
}
