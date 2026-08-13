//! ROCm / AMD GPU inventory from `rocm-smi` and `amd-smi` stdout.
//!
//! Probe commands (2s timeout, no native HIP/NVML):
//! - `rocm-smi --showmeminfo vram --csv` (PATH, `/opt/rocm/bin/rocm-smi`, `/usr/bin/rocm-smi`)
//! - `amd-smi metric --mem-usage --csv` then `--json` (PATH, `/opt/rocm/bin/amd-smi`)
//!
//! `amd-smi metric --mem-usage` is the documented memory-block query
//! (`-m` / `--mem-usage` in AMD SMI CLI). CSV flatten (amdsmi_logger) emits
//! `gpu,total_vram,used_vram,free_vram,...` with integer **MB** =
//! `bytes / 1024²` (same as nvidia-smi MiB). JSON wraps quantities as
//! `{"value": N, "unit": "MB"}`. Sample (one GPU, 32 GiB, 1 GiB used):
//!
//! ```text
//! gpu,total_vram,used_vram,free_vram
//! 0,32768,1024,31744
//! ```
//!
//! ```json
//! [{"gpu":0,"mem_usage":{"total_vram":{"value":32768,"unit":"MB"},"used_vram":{"value":1024,"unit":"MB"},"free_vram":{"value":31744,"unit":"MB"}}}]
//! ```
//!
//! Optional names: `rocm-smi --showproductname --csv` or
//! `amd-smi static --asic --csv` (`market_name`). VRAM does not wait on names.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ollama_capacity_types::{bytes_to_gib, mib_to_gib, GpuDetail};
use serde_json::Value;

use super::nvidia::{inventory_from_details, GpuInventory};

/// PATH plus well-known install locations (systemd PATH is thin).
pub fn rocm_smi_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("rocm-smi"),
        PathBuf::from("/opt/rocm/bin/rocm-smi"),
        PathBuf::from("/usr/bin/rocm-smi"),
    ]
}

/// Newer ROCm stacks deprecate `rocm-smi` in favor of `amd-smi`.
pub fn amd_smi_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("amd-smi"),
        PathBuf::from("/opt/rocm/bin/amd-smi"),
    ]
}

/// Parse `rocm-smi` CSV or `amd-smi` CSV/JSON stdout into inventory.
pub fn parse_rocm_csv(stdout: &str) -> Option<GpuInventory> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return parse_amd_smi_json(trimmed);
    }
    parse_rocm_table(trimmed)
}

/// Overlay marketing names from `--showproductname` / `static --asic` stdout.
pub fn apply_product_names(inv: &mut GpuInventory, stdout: &str) {
    let names = parse_product_names(stdout);
    if names.is_empty() {
        return;
    }
    for (i, gpu) in inv.gpus_detail.iter_mut().enumerate() {
        if let Some(name) = names.get(&gpu.index).or_else(|| names.get(&(i as i32))) {
            if !name.is_empty() {
                gpu.name = name.clone();
            }
        }
    }
    inv.gpu_names = inv.gpus_detail.iter().map(|gpu| gpu.name.clone()).collect();
}

fn parse_rocm_table(stdout: &str) -> Option<GpuInventory> {
    let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next()?;
    let first_cells: Vec<&str> = split_csv(first);
    if let Some(roles) = column_map(&first_cells) {
        let details: Vec<GpuDetail> = lines
            .enumerate()
            .filter_map(|(index, line)| parse_mapped_row(index as i32, line, &roles))
            .collect();
        return nonempty_inventory(details);
    }
    let mut details = Vec::new();
    if let Some(gpu) = parse_positional_line(0, first) {
        details.push(gpu);
    }
    for (index, line) in lines.enumerate() {
        if let Some(gpu) = parse_positional_line((index + 1) as i32, line) {
            details.push(gpu);
        }
    }
    nonempty_inventory(details)
}

fn nonempty_inventory(details: Vec<GpuDetail>) -> Option<GpuInventory> {
    if details.is_empty() {
        None
    } else {
        Some(inventory_from_details(details))
    }
}

fn split_csv(line: &str) -> Vec<&str> {
    line.split(',').map(str::trim).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Col {
    Index,
    Name,
    Total,
    Used,
    Free,
    UtilGpu,
    UtilMem,
    Temp,
    Power,
    Clock,
    Fan,
    Skip,
}

fn column_map(headers: &[&str]) -> Option<Vec<Col>> {
    if headers.is_empty() {
        return None;
    }
    let roles: Vec<Col> = headers
        .iter()
        .map(|header| classify_header(&normalize_header(header)))
        .collect();
    roles
        .iter()
        .any(|role| matches!(role, Col::Total | Col::Used | Col::Free))
        .then_some(roles)
}

fn normalize_header(raw: &str) -> String {
    let mut out = String::new();
    let mut underscore = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            underscore = false;
        } else if !out.is_empty() && !underscore {
            out.push('_');
            underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn classify_header(norm: &str) -> Col {
    match norm {
        "gpu" | "device" | "index" | "gpu_id" => Col::Index,
        "name" | "market_name" | "card_series" | "product_name" | "card_model" => Col::Name,
        "total_vram" | "vram_total" => Col::Total,
        "used_vram" | "vram_used" => Col::Used,
        "free_vram" | "vram_free" => Col::Free,
        "gfx" | "gfx_activity" | "gfx_util" | "gfx_busy" => Col::UtilGpu,
        "mem" | "umc_activity" | "mem_util" => Col::UtilMem,
        "hotspot_temperature" | "gpu_t" | "temperature" | "temperature_c" => Col::Temp,
        "power_usage" | "socket_power" | "power_draw_w" => Col::Power,
        "gfx_clk" | "clock_sm_mhz" => Col::Clock,
        "fan" | "fan_speed" | "fan_speed_pct" => Col::Fan,
        _ => classify_fuzzy_header(norm),
    }
}

fn classify_fuzzy_header(norm: &str) -> Col {
    if norm.contains("visible") || norm.contains("gtt") || norm.contains("percent") {
        return Col::Skip;
    }
    if (norm.contains("total") && norm.contains("used")) || norm.contains("used_memory") {
        Col::Used
    } else if norm.contains("free") && (norm.contains("vram") || norm.contains("memory")) {
        Col::Free
    } else if norm.contains("total") && (norm.contains("vram") || norm.contains("memory")) {
        Col::Total
    } else if norm.contains("series") || norm.contains("product") || norm.contains("market") {
        Col::Name
    } else {
        Col::Skip
    }
}

fn parse_mapped_row(fallback_index: i32, line: &str, roles: &[Col]) -> Option<GpuDetail> {
    let values: Vec<&str> = split_csv(line);
    if values.is_empty() {
        return None;
    }
    let mut index = fallback_index;
    let mut name = String::new();
    let mut total = None;
    let mut used = None;
    let mut free = None;
    let mut util = None;
    let mut mem_util = None;
    let mut temp = None;
    let mut power = None;
    let mut clock = None;
    let mut fan = None;
    for (i, role) in roles.iter().enumerate() {
        let Some(raw) = values.get(i).copied() else {
            continue;
        };
        match role {
            Col::Index => {
                if let Some(parsed) = parse_card_index(raw) {
                    index = parsed;
                }
                if name.is_empty() && looks_like_device_label(raw) {
                    name = raw.to_string();
                }
            }
            Col::Name => {
                if !raw.is_empty() && !raw.eq_ignore_ascii_case("n/a") {
                    name = raw.to_string();
                }
            }
            Col::Total => total = parse_rocm_memory(raw),
            Col::Used => used = parse_rocm_memory(raw),
            Col::Free => free = parse_rocm_memory(raw),
            Col::UtilGpu => util = parse_pct(raw),
            Col::UtilMem => mem_util = parse_pct(raw),
            Col::Temp => temp = parse_temp(raw),
            Col::Power => power = parse_power(raw),
            Col::Clock => clock = parse_clock(raw),
            Col::Fan => fan = parse_pct(raw),
            Col::Skip => {}
        }
    }
    Some(gpu_detail(
        index,
        fallback_name(name, values.first().copied().unwrap_or(""), fallback_index),
        MemSample {
            total: total?,
            used,
            free,
        },
        Telemetry {
            util,
            mem_util,
            temp,
            power,
            clock,
            fan,
        },
    ))
}

fn parse_positional_line(fallback_index: i32, line: &str) -> Option<GpuDetail> {
    let values: Vec<&str> = split_csv(line);
    if values.len() < 3 {
        return None;
    }
    let head = values[0].to_ascii_lowercase();
    if head.contains("device") || head.contains("gpu") || head.contains("vram") {
        return None;
    }
    let index = parse_card_index(values[0]).unwrap_or(fallback_index);
    Some(gpu_detail(
        index,
        fallback_name(String::new(), values[0], fallback_index),
        MemSample {
            total: parse_rocm_memory(values[1])?,
            used: parse_rocm_memory(values[2]),
            free: values.get(3).and_then(|raw| parse_rocm_memory(raw)),
        },
        Telemetry::default(),
    ))
}

struct MemSample {
    total: f64,
    used: Option<f64>,
    free: Option<f64>,
}

#[derive(Default)]
struct Telemetry {
    util: Option<f64>,
    mem_util: Option<f64>,
    temp: Option<f64>,
    power: Option<f64>,
    clock: Option<f64>,
    fan: Option<f64>,
}

fn gpu_detail(index: i32, name: String, mem: MemSample, tel: Telemetry) -> GpuDetail {
    let used_known = mem.used.is_some();
    let free_known = mem.free.is_some() || used_known;
    let used_gb = mem.used.unwrap_or(0.0);
    let free_gb = mem.free.unwrap_or((mem.total - used_gb).max(0.0));
    GpuDetail {
        index,
        name,
        vram_total_gb: mem.total,
        vram_used_gb: used_gb,
        vram_free_gb: free_gb,
        utilization_gpu_pct: tel.util,
        utilization_memory_pct: tel.mem_util,
        temperature_c: tel.temp,
        power_draw_w: tel.power,
        clock_sm_mhz: tel.clock,
        fan_speed_pct: tel.fan,
        vram_free_known: Some(free_known),
        vram_used_known: Some(used_known),
        util_known: Some(tel.util.is_some()),
    }
}

fn fallback_name(name: String, first_cell: &str, fallback_index: i32) -> String {
    if !name.is_empty() {
        return name;
    }
    if looks_like_device_label(first_cell) {
        first_cell.to_string()
    } else {
        format!("card{fallback_index}")
    }
}

fn looks_like_device_label(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("n/a") && trimmed.parse::<i32>().is_err()
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
    let (value, unit) = split_number_unit(raw)?;
    quantity_to_gib(value, unit.as_deref())
}

fn parse_pct(raw: &str) -> Option<f64> {
    parse_scalar(raw).filter(|v| (0.0..=100.0).contains(v))
}

fn parse_temp(raw: &str) -> Option<f64> {
    parse_scalar(raw).filter(|v| (-40.0..=150.0).contains(v))
}

fn parse_power(raw: &str) -> Option<f64> {
    parse_scalar(raw).filter(|v| (0.0..=2000.0).contains(v))
}

fn parse_clock(raw: &str) -> Option<f64> {
    parse_scalar(raw).filter(|v| (0.0..=5000.0).contains(v))
}

fn parse_scalar(raw: &str) -> Option<f64> {
    split_number_unit(raw)
        .map(|(value, _)| value)
        .filter(|v| v.is_finite())
}

fn split_number_unit(raw: &str) -> Option<(f64, Option<String>)> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() || raw.eq_ignore_ascii_case("n/a") || raw == "-" {
        return None;
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut end = 0;
    if matches!(chars.first(), Some('+')) {
        end = 1;
    }
    while end < chars.len()
        && (chars[end].is_ascii_digit() || chars[end] == '.' || chars[end] == '_')
    {
        end += 1;
    }
    if end == 0 || (end == 1 && chars[0] == '+') {
        return None;
    }
    let number: String = chars[..end].iter().copied().filter(|c| *c != '_').collect();
    let value = number
        .parse::<f64>()
        .ok()
        .filter(|v| *v >= 0.0 && v.is_finite())?;
    let unit: String = chars[end..].iter().collect::<String>();
    let unit = unit.trim();
    let unit = if unit.is_empty() {
        None
    } else {
        Some(unit.to_string())
    };
    Some((value, unit))
}

fn quantity_to_gib(value: f64, unit: Option<&str>) -> Option<f64> {
    let Some(unit) = unit else {
        if value >= 1_048_576.0 {
            return Some(bytes_to_gib(value as u64));
        }
        return Some(mib_to_gib(value));
    };
    let normalized = unit
        .trim()
        .trim_start_matches('%')
        .trim()
        .trim_end_matches('s')
        .to_ascii_lowercase();
    match normalized.as_str() {
        "b" | "byte" => Some(bytes_to_gib(value as u64)),
        "kib" | "kb" => Some(value / (1024.0 * 1024.0)),
        "mib" | "mb" | "m" => Some(mib_to_gib(value)),
        "gib" | "gb" | "g" => Some(value),
        "tib" | "tb" | "t" => Some(value * 1024.0),
        _ => None,
    }
}

fn parse_amd_smi_json(raw: &str) -> Option<GpuInventory> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let objects = json_gpu_objects(&value)?;
    let details: Vec<GpuDetail> = objects
        .iter()
        .enumerate()
        .filter_map(|(index, obj)| parse_amd_smi_json_gpu(index as i32, obj))
        .collect();
    nonempty_inventory(details)
}

fn json_gpu_objects(value: &Value) -> Option<Vec<&Value>> {
    match value {
        Value::Array(items) => Some(items.iter().collect()),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("gpu_data") {
                Some(items.iter().collect())
            } else if map.contains_key("gpu")
                || map.contains_key("mem_usage")
                || map.contains_key("vram")
            {
                Some(vec![value])
            } else {
                None
            }
        }
        _ => None,
    }
}

fn parse_amd_smi_json_gpu(fallback_index: i32, obj: &Value) -> Option<GpuDetail> {
    let index = obj
        .get("gpu")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .unwrap_or(fallback_index);
    let total = json_mem(obj, &["mem_usage", "total_vram"])
        .or_else(|| json_mem(obj, &["vram", "size"]))
        .or_else(|| json_mem(obj, &["vram_total"]))
        .or_else(|| json_mem(obj, &["total_vram"]))?;
    let used = json_mem(obj, &["mem_usage", "used_vram"])
        .or_else(|| json_mem(obj, &["vram_used"]))
        .or_else(|| json_mem(obj, &["used_vram"]));
    let free = json_mem(obj, &["mem_usage", "free_vram"])
        .or_else(|| json_mem(obj, &["vram_free"]))
        .or_else(|| json_mem(obj, &["free_vram"]));
    let util = json_scalar(obj, &["usage", "gfx_activity"])
        .or_else(|| json_scalar(obj, &["gfx"]))
        .filter(|v| (0.0..=100.0).contains(v));
    let mem_util = json_scalar(obj, &["usage", "umc_activity"])
        .or_else(|| json_scalar(obj, &["mem"]))
        .filter(|v| (0.0..=100.0).contains(v));
    let temp = json_scalar(obj, &["hotspot_temperature"]).filter(|v| (-40.0..=150.0).contains(v));
    let power = json_scalar(obj, &["power_usage"])
        .or_else(|| json_scalar(obj, &["power", "socket_power"]))
        .filter(|v| (0.0..=2000.0).contains(v));
    let clock = json_scalar(obj, &["gfx_clk"]).filter(|v| (0.0..=5000.0).contains(v));
    let name = json_string(obj, &["asic", "market_name"])
        .or_else(|| json_string(obj, &["market_name"]))
        .or_else(|| json_string(obj, &["board", "product_name"]))
        .unwrap_or_else(|| format!("card{index}"));
    Some(gpu_detail(
        index,
        name,
        MemSample { total, used, free },
        Telemetry {
            util,
            mem_util,
            temp,
            power,
            clock,
            fan: None,
        },
    ))
}

fn json_mem(obj: &Value, path: &[&str]) -> Option<f64> {
    let (value, unit) = json_quantity_at(obj, path)?;
    quantity_to_gib(value, unit.as_deref())
}

fn json_scalar(obj: &Value, path: &[&str]) -> Option<f64> {
    json_quantity_at(obj, path)
        .map(|(value, _)| value)
        .filter(|v| v.is_finite())
}

fn json_string(obj: &Value, path: &[&str]) -> Option<String> {
    let mut cur = obj;
    for key in path {
        cur = cur.get(*key)?;
    }
    cur.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("n/a"))
        .map(str::to_string)
}

fn json_quantity_at(obj: &Value, path: &[&str]) -> Option<(f64, Option<String>)> {
    let mut cur = obj;
    for key in path {
        cur = cur.get(*key)?;
    }
    json_quantity(cur)
}

fn json_quantity(value: &Value) -> Option<(f64, Option<String>)> {
    match value {
        Value::Number(number) => number.as_f64().filter(|v| *v >= 0.0).map(|v| (v, None)),
        Value::String(raw) => split_number_unit(raw),
        Value::Object(map) => {
            let inner = map.get("value")?;
            let unit = map
                .get("unit")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            match inner {
                Value::Number(number) => number.as_f64().filter(|v| *v >= 0.0).map(|v| (v, unit)),
                Value::String(raw) => {
                    let (n, parsed_unit) = split_number_unit(raw)?;
                    Some((n, unit.or(parsed_unit)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_product_names(stdout: &str) -> BTreeMap<i32, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return BTreeMap::new();
    }
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        return parse_product_names_json(trimmed);
    }
    parse_product_names_csv(trimmed)
}

fn parse_product_names_csv(stdout: &str) -> BTreeMap<i32, String> {
    let mut names = BTreeMap::new();
    let mut lines = stdout.lines().filter(|line| !line.trim().is_empty());
    let Some(header_line) = lines.next() else {
        return names;
    };
    let headers: Vec<String> = split_csv(header_line)
        .into_iter()
        .map(normalize_header)
        .collect();
    let name_cols: Vec<(usize, u8)> = headers
        .iter()
        .enumerate()
        .filter_map(|(i, header)| product_name_priority(header).map(|p| (i, p)))
        .collect();
    if name_cols.is_empty() {
        return names;
    }
    let index_col = headers
        .iter()
        .position(|header| matches!(header.as_str(), "gpu" | "device" | "index" | "gpu_id"));
    for (fallback, line) in lines.enumerate() {
        let values = split_csv(line);
        let index = index_col
            .and_then(|col| values.get(col).copied())
            .and_then(parse_card_index)
            .or_else(|| values.first().copied().and_then(parse_card_index))
            .unwrap_or(fallback as i32);
        let mut best: Option<(u8, String)> = None;
        for (col, priority) in &name_cols {
            let Some(raw) = values.get(*col).map(|s| s.trim()) else {
                continue;
            };
            if raw.is_empty() || raw.eq_ignore_ascii_case("n/a") {
                continue;
            }
            if best.as_ref().is_none_or(|(p, _)| *priority < *p) {
                best = Some((*priority, raw.to_string()));
            }
        }
        if let Some((_, name)) = best {
            names.insert(index, name);
        }
    }
    names
}

fn product_name_priority(norm: &str) -> Option<u8> {
    Some(match norm {
        "market_name" => 0,
        "card_series" => 1,
        "product_name" => 2,
        "name" => 3,
        "card_model" => 4,
        _ if norm.contains("series") || norm.contains("product") || norm.contains("market") => 5,
        _ => return None,
    })
}

fn parse_product_names_json(raw: &str) -> BTreeMap<i32, String> {
    let mut names = BTreeMap::new();
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return names;
    };
    let Some(objects) = json_gpu_objects(&value) else {
        return names;
    };
    for (fallback, obj) in objects.iter().enumerate() {
        let index = obj
            .get("gpu")
            .and_then(Value::as_i64)
            .map(|v| v as i32)
            .unwrap_or(fallback as i32);
        if let Some(name) = json_string(obj, &["asic", "market_name"])
            .or_else(|| json_string(obj, &["market_name"]))
            .or_else(|| json_string(obj, &["board", "product_name"]))
        {
            names.insert(index, name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use ollama_capacity_types::BYTES_PER_GIB;
    use std::path::Path;

    const BYTES_32_GIB: u64 = 32 * 1024 * 1024 * 1024;

    #[test]
    fn candidates_include_path_and_well_known() {
        let rocm = rocm_smi_candidates();
        assert_eq!(rocm[0], PathBuf::from("rocm-smi"));
        assert!(rocm
            .iter()
            .any(|p| p.as_path() == Path::new("/opt/rocm/bin/rocm-smi")));
        assert!(rocm
            .iter()
            .any(|p| p.as_path() == Path::new("/usr/bin/rocm-smi")));
        let amd = amd_smi_candidates();
        assert_eq!(amd[0], PathBuf::from("amd-smi"));
        assert!(amd
            .iter()
            .any(|p| p.as_path() == Path::new("/opt/rocm/bin/amd-smi")));
    }

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
    fn parses_mib_and_gib_suffixes() {
        let mib = "device,VRAM Total Memory,VRAM Total Used Memory\ncard0,24576 MiB,1024 MiB\n";
        let inv = parse_rocm_csv(mib).expect("mib");
        assert!((inv.vram_gb - 24.0).abs() < 1e-6);
        assert!((inv.vram_used_gb - 1.0).abs() < 1e-6);
        let gib = "device,VRAM Total Memory,VRAM Total Used Memory\ncard0,24 GiB,1 GiB\n";
        let inv = parse_rocm_csv(gib).expect("gib");
        assert!((inv.vram_gb - 24.0).abs() < 1e-6);
        assert!((inv.vram_used_gb - 1.0).abs() < 1e-6);
    }

    #[test]
    fn full_gpu_free_zero_is_known() {
        let csv = "device,VRAM Total Memory (B),VRAM Total Used Memory (B)\ncard0,25769803776,25769803776\n";
        let inv = parse_rocm_csv(csv).expect("full");
        assert!(inv.vram_free_known);
        assert!((inv.vram_free_gb - 0.0).abs() < 1e-12);
        assert_eq!(inv.gpus_detail[0].vram_free_known, Some(true));
        assert_eq!(inv.gpus_detail[0].util_known, Some(false));
        assert!(inv.gpus_detail[0].utilization_gpu_pct.is_none());
    }

    #[test]
    fn empty_is_none() {
        assert!(parse_rocm_csv("").is_none());
        assert!(
            parse_rocm_csv("device,VRAM Total Memory (B),VRAM Total Used Memory (B)\n").is_none()
        );
    }

    #[test]
    fn bytes_32_gib_is_32() {
        assert!((bytes_to_gib(BYTES_32_GIB) - 32.0).abs() < 1e-9);
        assert!(((32.0 * BYTES_PER_GIB) as u64).abs_diff(BYTES_32_GIB) < 2);
        let csv = format!(
            "device,VRAM Total Memory (B),VRAM Total Used Memory (B)\ncard0,{BYTES_32_GIB},0\n"
        );
        let inv = parse_rocm_csv(&csv).expect("32gib");
        assert!((inv.vram_gb - 32.0).abs() < 1e-6);
        assert!(inv.vram_free_known);
    }

    #[test]
    fn parses_amd_smi_metric_mem_usage_csv() {
        // `amd-smi metric --mem-usage --csv` (AMD SMI CLI; MB = bytes/1024²).
        let csv = "gpu,total_vram,used_vram,free_vram,total_visible_vram,used_visible_vram,free_visible_vram,total_gtt,used_gtt,free_gtt\n0,32768,1024,31744,32768,1024,31744,0,0,0\n";
        let inv = parse_rocm_csv(csv).expect("amd-smi csv");
        assert_eq!(inv.gpus, 1);
        assert_eq!(inv.gpus_detail[0].index, 0);
        assert!((inv.vram_gb - 32.0).abs() < 1e-6);
        assert!((inv.vram_used_gb - 1.0).abs() < 1e-6);
        assert!((inv.vram_free_gb - 31.0).abs() < 1e-6);
        assert!(inv.vram_free_known);
        assert!(inv.vram_used_known);
    }

    #[test]
    fn parses_amd_smi_metric_mem_usage_json() {
        let json = r#"[{"gpu":0,"mem_usage":{"total_vram":{"value":32768,"unit":"MB"},"used_vram":{"value":1024,"unit":"MB"},"free_vram":{"value":31744,"unit":"MB"}}}]"#;
        let inv = parse_rocm_csv(json).expect("amd-smi json");
        assert_eq!(inv.gpus, 1);
        assert!((inv.vram_gb - 32.0).abs() < 1e-6);
        assert!((inv.vram_used_gb - 1.0).abs() < 1e-6);
        assert!(inv.vram_free_known);
        assert_eq!(inv.gpus_detail[0].util_known, Some(false));
    }

    #[test]
    fn parses_optional_monitor_telemetry_when_columns_present() {
        // `amd-smi monitor --vram-usage --gfx --temperature --power-usage --csv`
        let csv = "gpu,gfx,vram_used,vram_free,vram_total,hotspot_temperature,power_usage\n0,14,1024,31744,32768,62,120.5\n";
        let inv = parse_rocm_csv(csv).expect("monitor csv");
        let gpu = &inv.gpus_detail[0];
        assert!((inv.vram_gb - 32.0).abs() < 1e-6);
        assert!(inv.vram_free_known);
        assert_eq!(gpu.util_known, Some(true));
        assert!((gpu.utilization_gpu_pct.unwrap() - 14.0).abs() < 1e-9);
        assert!((gpu.temperature_c.unwrap() - 62.0).abs() < 1e-9);
        assert!((gpu.power_draw_w.unwrap() - 120.5).abs() < 1e-9);
    }

    #[test]
    fn missing_util_column_stays_unknown() {
        let csv = "gpu,total_vram,used_vram,free_vram\n0,32768,1024,31744\n";
        let inv = parse_rocm_csv(csv).expect("csv");
        assert_eq!(inv.gpus_detail[0].util_known, Some(false));
        assert!(inv.gpus_detail[0].utilization_gpu_pct.is_none());
    }

    #[test]
    fn product_names_overlay_card_label() {
        let csv = "gpu,total_vram,used_vram,free_vram\n0,32768,1024,31744\n";
        let mut inv = parse_rocm_csv(csv).expect("csv");
        apply_product_names(&mut inv, "gpu,market_name\n0,AMD Instinct MI300A\n");
        assert_eq!(inv.gpus_detail[0].name, "AMD Instinct MI300A");
        assert_eq!(inv.gpu_names, ["AMD Instinct MI300A"]);
    }

    #[test]
    fn showproductname_csv_fills_name() {
        let csv = "gpu,total_vram,used_vram\n0,16384,1024\n";
        let mut inv = parse_rocm_csv(csv).expect("csv");
        apply_product_names(
            &mut inv,
            "device,Card series,Card model,Card vendor,Card SKU\ncard0,AMD Radeon RX 7900 XTX,0x744c,Advanced Micro Devices,XTYH\n",
        );
        assert_eq!(inv.gpus_detail[0].name, "AMD Radeon RX 7900 XTX");
    }
}
