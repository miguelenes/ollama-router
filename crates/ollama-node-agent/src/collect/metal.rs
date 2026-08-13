//! Apple Metal display inventory. Names and count only — never unified RAM as VRAM.

use serde_json::Value;

use super::nvidia::GpuInventory;

/// `system_profiler SPDisplaysDataType -json` → names + GPU count, no fake VRAM.
pub fn parse_metal_displays_json(raw: &str) -> Option<GpuInventory> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let displays = value.get("SPDisplaysDataType")?.as_array()?;
    let mut names = Vec::new();
    for display in displays {
        let name = display
            .get("_name")
            .and_then(Value::as_str)
            .or_else(|| display.get("sppci_model").and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        return None;
    }
    Some(GpuInventory {
        gpus: names.len() as i32,
        gpu_names: names,
        vram_gb: 0.0,
        vram_used_gb: 0.0,
        vram_free_gb: 0.0,
        vram_free_known: false,
        vram_used_known: false,
        gpus_detail: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apple_m4_display() {
        let raw = r#"{"SPDisplaysDataType":[{"_name":"Apple M4","sppci_model":"Apple M4"}]}"#;
        let inv = parse_metal_displays_json(raw).expect("json");
        assert_eq!(inv.gpus, 1);
        assert_eq!(inv.gpu_names, ["Apple M4"]);
        assert!(!inv.vram_free_known);
        assert!((inv.vram_gb - 0.0).abs() < 1e-12);
    }

    #[test]
    fn empty_is_none() {
        assert!(parse_metal_displays_json("{}").is_none());
        assert!(parse_metal_displays_json(r#"{"SPDisplaysDataType":[]}"#).is_none());
    }
}
