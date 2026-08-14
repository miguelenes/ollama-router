//! YAML tunables + env knobs; reject top-level `nodes:`.
//!
//! Inventory is `OLLAMA_ROUTER_FLEET` + FleetState + Verda.

pub(crate) mod env_source;
pub(crate) mod error;
pub(crate) mod knobs;
pub(crate) mod load;
pub(crate) mod merge;
pub(crate) mod models;

pub use env_source::{EnvSource, OsEnv};
pub use error::ConfigError;
pub use load::{hydrate_node_urls, load_config, load_config_from, parse_yaml, DEFAULTS_YAML};
pub use models::{
    http_url_for_bind, socket_addr_for_bind, Capacity, HealthConfig, ModelTier, NodeConfig,
    PolicyConfig, RequestClass, RouterConfig, SelectionStrategy, TimeoutsConfig, TunnelConfig,
    UpstreamPoolConfig, VerdaConfig, YamlTunables,
};
