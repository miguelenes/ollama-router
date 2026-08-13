//! rocm-smi CSV → [`GpuInventory`]. Two known `--showmeminfo vram --csv` layouts.

use ollama_capacity_types::{bytes_to_gib, mib_to_gib, GpuDetail};

use super::nvidia::{inventory_from_details, GpuInventory};

/// Parse `rocm-smi --showmeminfo vram --csv` stdout.
pub fn parse_rocm_csv(stdout: &str) -> Option<GpuInventory> {
    let details: Vec<GpuDetail> = stdout
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_rocm_line(index as i32, line))
        .collect();
    if details.is_empty() {
        None
    } else {
        Some(inventory_from_details(details))
    }
}

fn parse_rocm_line(fallback_index: i32, line: &str) -> Option<GpuDetail> {
    let values: Vec<&str> = line.split(',').map(str::trim).collect();
    if values.len() < 3 {
        return None;
    }
    let head = values[0].to_ascii_lowercase();
    if head.contains("device") || head.contains("gpu") || head.contains("vram") {
        return None;
    }
    let index = parse_card_index(values[0]).unwrap_or(fallback_index);
    let total = parse_rocm_memory(values[1])?;
    let used = parse_rocm_memory(values[2]);
    let used_known = used.is_some();
    let used_gb = used.unwrap_or(0.0);
    let free_gb = (total - used_gb).max(0.0);
    Some(GpuDetail {
        index,
        name: values[0].to_string(),
        vram_total_gb: total,
        vram_used_gb: used_gb,
        vram_free_gb: free_gb,
        vram_free_known: Some(used_known),
        vram_used_known: Some(used_known),
        util_known: Some(false),
        ..GpuDetail::default()
    })
}

fn parse_card_index(raw: &str) -> Option<i32> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn parse_rocm_memory(raw: &str) -> Option<f64> {
    let cleaned = raw
        .trim()
        .trim_end_matches("B")
        .trim_end_matches("i")
        .trim();
    let value = cleaned.parse::<f64>().ok().filter(|v| *v >= 0.0)?;
    if value >= 1_048_576.0 {
        Some(bytes_to_gib(value as u64))
    } else {
        Some(mib_to_gib(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_bytes_layout() {
        let csv = "device,VRAM Total Memory (B),VRAM Total Used Memory (B)\ncard0,25769803776,1073741824\n";
        let inv = parse_rocm_csv(csv).expect("csv");
        assert_eq!(inv.gpus, 1);
        assert!((inv.vram_gb - 24.0).abs() < 1e-6);
        assert!(inv.vram_free_known);
        assert!(inv.vram_used_known);
        assert!((inv.vram_used_gb - 1.0).abs() < 1e-6);
    }

    #[test]
    fn parses_gpu_index_layout() {
        let csv =
            "GPU,VRAM Total Memory (B),VRAM Total Used Memory (B)\n0,17179869184,2147483648\n";
        let inv = parse_rocm_csv(csv).expect("csv");
        assert_eq!(inv.gpus, 1);
        assert_eq!(inv.gpus_detail[0].index, 0);
        assert!((inv.vram_gb - 16.0).abs() < 1e-6);
    }

    #[test]
    fn empty_is_none() {
        assert!(parse_rocm_csv("").is_none());
        assert!(
            parse_rocm_csv("device,VRAM Total Memory (B),VRAM Total Used Memory (B)\n").is_none()
        );
    }
}
