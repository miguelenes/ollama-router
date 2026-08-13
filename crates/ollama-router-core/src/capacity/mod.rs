//! HTTP client for the node-local agent (`:11436`).
//!
//! GiB = bytes / `1024³`. Soft-fail: callers never flip health from a miss.
//! The agent lives in `crates/ollama-node-agent`. This module is the client + merge.

mod client;
mod merge;
mod types;

pub use client::{capacity_target, CapacityClient, CapacityError, CapacityProbe, CapacityTarget};
pub use merge::{merge_capacity, CapacitySource, MergeOutcome};
pub use types::{
    bytes_to_gib, CapacityInventory, CapacityReport, GpuBackend, GpuDetail, Pressure,
    PressureEnvelope, BYTES_PER_GIB,
};

#[cfg(test)]
mod tests;
