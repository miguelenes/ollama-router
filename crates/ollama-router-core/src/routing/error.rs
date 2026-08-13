//! Typed routing rejection. Wire JSON reason strings stay Python-stable.

use std::fmt;

/// Why no candidate could be selected. Wire `reason:` values must not change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutingError {
    NoNodes,
    NoHealthy,
    ModelMissing,
    Capacity,
    Ram,
    RamPressure,
    Saturated,
}

impl RoutingError {
    /// Python-stable reason token embedded in Ollama-shaped JSON.
    pub fn as_reason_code(self) -> &'static str {
        match self {
            Self::NoNodes => "no_nodes_configured",
            Self::NoHealthy => "no_healthy_nodes",
            Self::ModelMissing => "model_missing",
            Self::Capacity => "insufficient_capacity",
            Self::Ram => "insufficient_ram",
            Self::RamPressure => "ram_pressure",
            Self::Saturated => "all_nodes_saturated",
        }
    }

    /// Human message (without the `ollama-router:` prefix).
    pub fn message(self) -> &'static str {
        match self {
            Self::NoNodes => "no ollama nodes configured",
            Self::NoHealthy => "no healthy ollama nodes",
            Self::ModelMissing => "model not present on any healthy node",
            Self::Capacity => "no node with sufficient capacity for this model class",
            Self::Ram => "no node with sufficient system RAM for this model class",
            Self::RamPressure => "all candidate nodes are under RAM pressure",
            Self::Saturated => {
                "all eligible nodes are saturated (inflight cap reached); retry shortly"
            }
        }
    }

    /// `Retry-After` seconds for capacity-miss 503s. `model_missing` has none.
    pub fn retry_after_seconds(self, policy: &crate::config::PolicyConfig) -> Option<u32> {
        match self {
            Self::ModelMissing => None,
            Self::Capacity => Some(policy.provision_retry_after_seconds),
            Self::NoNodes | Self::NoHealthy | Self::Ram | Self::RamPressure | Self::Saturated => {
                Some(policy.saturated_retry_after_seconds)
            }
        }
    }

    /// Whether this miss should kick async demand scale-up.
    pub fn requests_demand_scale_up(self) -> bool {
        !matches!(self, Self::ModelMissing)
    }
}

impl fmt::Display for RoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for RoutingError {}
