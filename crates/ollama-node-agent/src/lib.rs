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
pub mod time_util;
pub mod uninstall;
#[cfg(windows)]
pub mod windows_scm;

pub use config::AgentConfig;
pub use http::{make_app, AppState};

/// Build a rustls-only reqwest client. Never falls back to `Client::new()`.
///
/// Mirrors `ollama-router-core::http_util::rustls_client` without taking a
/// dependency on core. `connect` sets `connect_timeout`; `request` sets the
/// per-request `timeout`.
pub(crate) fn rustls_client(
    connect: Option<std::time::Duration>,
    request: Option<std::time::Duration>,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder().use_rustls_tls();
    if let Some(timeout) = connect {
        builder = builder.connect_timeout(timeout);
    }
    if let Some(timeout) = request {
        builder = builder.timeout(timeout);
    }
    builder.build()
}

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
