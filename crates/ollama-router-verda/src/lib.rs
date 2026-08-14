//! Verda Cloud NVIDIA spot client, selector, and fleet manager.
//!
//! Core owns the cloud trait; this crate implements it.

mod client;
mod images;
mod keys;
mod manager;
mod selector;
mod startup;
mod types;

pub use client::{VerdaClient, VerdaError};
pub use images::pick_ubuntu24_nvidia_docker_image;
pub use keys::ensure_ssh_key_id;
pub use manager::{VerdaManager, MANAGED_BY};
pub use selector::{glob_match, pick_cheapest_available_spot_gpu, rank_candidates, SpotChoice};
pub use types::{Image, Instance, InstanceAvailability, InstanceType, SshKey, StartupScript};

pub use ollama_router_core as core;

#[cfg(test)]
mod tests;
