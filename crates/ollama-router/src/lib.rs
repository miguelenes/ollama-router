//! Ollama-compatible fleet proxy binary and HTTP surface.

pub mod cli;
pub mod http;
pub mod provision;
pub mod proxy;

pub use ollama_router_core as core;
pub use ollama_router_verda as verda;
