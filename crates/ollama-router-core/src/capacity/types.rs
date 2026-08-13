//! Re-export shared capacity DTOs and convert them into static inventory.

pub use ollama_capacity_types::{
    bytes_to_gib, CapacityReport, GpuBackend, GpuDetail, Pressure, PressureEnvelope, BYTES_PER_GIB,
};

use crate::config::Capacity;

/// Convert a capacity report into tunables-style inventory numbers.
pub trait CapacityInventory {
    fn as_capacity(&self) -> Capacity;
}

impl CapacityInventory for CapacityReport {
    fn as_capacity(&self) -> Capacity {
        Capacity {
            vram_gb: Some(self.vram_gb),
            ram_gb: Some(self.ram_gb),
            gpus: Some(self.gpus.max(0) as u32),
            cpu_cores: Some(self.cpu_cores.max(0) as u32),
        }
    }
}
