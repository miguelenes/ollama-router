//! RunPod interruptible GPU pod client, selector, and fleet manager.

mod client;
mod manager;
mod selector;
mod startup;
mod types;

pub use client::{RunpodClient, RunpodError};
pub use manager::{RunpodManager, MANAGED_BY_MARKER};
pub use selector::{rank_gpu_types, GpuChoice};
pub use types::{CatalogGpu, CreatePodRequest, Pod};

pub use ollama_router_core as core;

#[cfg(test)]
mod tests;
