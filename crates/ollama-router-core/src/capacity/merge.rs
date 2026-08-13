//! Pure merge of static inventory, agent discovery, and `/api/ps` VRAM floor.

use crate::config::Capacity;

/// Bytes in one gibibyte. sysinfo / Ollama `size` fields are bytes.
pub const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Convert a byte count to GiB (`bytes / 1024³`). Never divide by `1024²`.
pub fn bytes_to_gib(bytes: u64) -> f64 {
    bytes as f64 / BYTES_PER_GIB
}

/// Where effective inventory numbers came from.
///
/// Not Python `"configured"` / `"merged"` / `"ps_lower_bound"`: a `/api/ps`
/// floor does not change this tag.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CapacitySource {
    Agent,
    Static,
    #[default]
    Unknown,
}

impl CapacitySource {
    /// Python-adjacent token for logs (`agent` / `static` / `unknown`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Static => "static",
            Self::Unknown => "unknown",
        }
    }
}

/// Result of [`merge_capacity`].
#[derive(Clone, Debug, PartialEq)]
pub struct MergeOutcome {
    pub capacity: Capacity,
    pub source: CapacitySource,
}

fn any_static(static_cap: &Capacity) -> bool {
    static_cap.vram_gb.is_some()
        || static_cap.ram_gb.is_some()
        || static_cap.gpus.is_some()
        || static_cap.cpu_cores.is_some()
}

fn merge_capped(static_v: Option<f64>, discovered_v: Option<f64>) -> Option<f64> {
    match (static_v, discovered_v) {
        (Some(configured), Some(discovered)) => {
            if discovered > 0.0 {
                Some(configured.min(discovered))
            } else {
                Some(configured)
            }
        }
        (Some(configured), None) => Some(configured),
        (None, Some(discovered)) => Some(discovered),
        (None, None) => None,
    }
}

fn merge_override<T: Copy>(static_v: Option<T>, discovered_v: Option<T>) -> Option<T> {
    static_v.or(discovered_v)
}

/// Fill omitted static fields from discovery; explicit static VRAM/RAM **cap**
/// a positive discovery; explicit GPU/cores **override**. Both absent → unknown.
///
/// Positive `/api/ps` loaded VRAM is only a lower bound on effective VRAM
/// (`ceil`); source stays `agent`, `static`, or `unknown`.
pub fn merge_capacity(
    static_cap: &Capacity,
    discovered: Option<&Capacity>,
    loaded_vram_gb: Option<f64>,
) -> MergeOutcome {
    let disc = discovered;
    let mut capacity = Capacity {
        vram_gb: merge_capped(static_cap.vram_gb, disc.and_then(|d| d.vram_gb)),
        ram_gb: merge_capped(static_cap.ram_gb, disc.and_then(|d| d.ram_gb)),
        gpus: merge_override(static_cap.gpus, disc.and_then(|d| d.gpus)),
        cpu_cores: merge_override(static_cap.cpu_cores, disc.and_then(|d| d.cpu_cores)),
    };

    let source = if disc.is_some() {
        CapacitySource::Agent
    } else if any_static(static_cap) {
        CapacitySource::Static
    } else {
        CapacitySource::Unknown
    };

    if capacity.vram_gb() <= 0.0 {
        if let Some(loaded) = loaded_vram_gb {
            if loaded > 0.0 {
                let floor = loaded.ceil();
                capacity.vram_gb = Some(capacity.vram_gb().max(floor));
                capacity.gpus = Some(capacity.gpus().max(1));
            }
        }
    }

    MergeOutcome { capacity, source }
}
