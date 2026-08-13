//! YAML tunables + env knobs; reject top-level `nodes:`.
//!
//! Inventory is `OLLAMA_HOST_NN_*` + FleetState + Verda. Tests may use
//! `OLLAMA_ROUTER_NODES`.

pub(crate) mod env_source;
pub(crate) mod error;
pub(crate) mod knobs;
pub(crate) mod load;
pub(crate) mod merge;
pub(crate) mod models;
pub(crate) mod nodes_env;

pub use env_source::{EnvSource, OsEnv};
pub use error::ConfigError;
pub use load::{load_config, load_config_from, parse_yaml, DEFAULTS_YAML};
pub use models::{
    Capacity, HealthConfig, ModelTier, NodeConfig, NodeProvisionConfig, NodeSshConfig,
    PolicyConfig, ProvisionDefaults, RequestClass, RouterConfig, SelectionStrategy, TimeoutsConfig,
    VerdaConfig, YamlTunables,
};
pub use nodes_env::parse_nodes_env;
