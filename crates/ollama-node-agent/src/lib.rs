//! Node-local Ollama installer, supervisor, and `:11436` capacity agent.

pub mod cli;
pub mod collect;
pub mod config;
pub mod doctor;
pub mod http;
pub mod listen;
pub mod metrics;
pub mod redact;
pub mod register;
pub mod setup;
pub mod uninstall;

pub use config::AgentConfig;
pub use http::{make_app, AppState};
