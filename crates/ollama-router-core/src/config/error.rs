//! Config loading errors.

use crate::fleet::FleetStateError;

/// Raised for unparseable or rejected router configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Top-level YAML `nodes:` is not supported (empty list included).
    #[error(
        "{origin}: top-level 'nodes:' is not supported. \
         Permanent membership belongs in OLLAMA_ROUTER_FLEET (fleet.yaml). \
         Use this YAML only for tunables (policy, health, timeouts, verda, …)."
    )]
    NodesInventory { origin: String },
    /// YAML syntax or structure is invalid.
    #[error("invalid YAML: {0}")]
    InvalidYaml(String),
    /// Config root was not a mapping.
    #[error("config root must be a mapping")]
    RootNotMapping,
    /// Serde / semantic validation failed.
    #[error("invalid config: {0}")]
    Invalid(String),
    /// Durable fleet-state could not be read safely.
    #[error(transparent)]
    FleetState(#[from] FleetStateError),
    /// Filesystem error while reading an overlay.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ConfigError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}
