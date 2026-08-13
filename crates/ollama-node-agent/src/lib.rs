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
pub mod service_identity;
pub mod setup;
pub mod uninstall;
#[cfg(windows)]
pub mod windows_scm;

pub use config::AgentConfig;
pub use http::{make_app, AppState};

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ollama_node_agent=info".into()),
        )
        .init();
}
