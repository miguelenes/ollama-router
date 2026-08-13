//! Verda Cloud NVIDIA spot client, selector, and fleet manager.
//!
//! TODO: OAuth2 client, cheapest-spot selector, manager (`delete_permanently`).
//! Core owns the cloud trait; this crate implements it. Do not reverse that edge.

pub use ollama_router_core as core;
