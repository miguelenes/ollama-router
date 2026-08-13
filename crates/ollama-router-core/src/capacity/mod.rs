//! HTTP client for the sibling ollama-capacity-agent (`:11436`).
//!
//! GiB = bytes / `1024³`. Soft-fail: callers never flip health from a miss.
//! Do not reimplement the agent here.

mod client;
mod merge;
mod types;

pub use client::{capacity_target, CapacityClient, CapacityError, CapacityProbe, CapacityTarget};
pub use merge::{bytes_to_gib, merge_capacity, CapacitySource, MergeOutcome, BYTES_PER_GIB};
pub use types::{CapacityReport, GpuDetail, Pressure, PressureEnvelope};

#[cfg(test)]
mod tests;
