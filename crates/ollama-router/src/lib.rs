//! Ollama-compatible fleet proxy binary and HTTP surface.

pub mod bootstrap;
pub mod cli;
pub mod health;
pub mod http;
pub mod proxy;
pub mod tunnel;
pub mod warm;

pub use ollama_router_core as core;
pub use ollama_router_verda as verda;
