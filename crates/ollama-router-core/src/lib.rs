//! Shared routing, fleet, config, capacity, cloud, and job types.

pub mod capacity;
pub mod cloud;
pub mod config;
pub mod fleet;
pub mod jobs;
pub mod routing;

pub use cloud::{DemandScale, NoopDemandScale};
pub use config::{
    hydrate_node_urls, load_config, load_config_from, parse_yaml, ConfigError, EnvSource, OsEnv,
    RouterConfig,
};
pub use fleet::{
    load_fleet_nodes, parse_fleet_yaml, FleetState, FleetStateError, NodeId, NodeOrigin,
    NodeSnapshot, PressureLevel, Registry, RouterId, VerdaInstanceId, VerdaNodePersist,
    DEFAULT_FLEET_PATH, DEFAULT_STATE_PATH,
};
pub use jobs::{JobOutcome, JobStatus, ModelOrchestrator, OrchestratorError, StubOrchestrator};
pub use routing::{
    classify, parse_model_size_b, rank_nodes, RankOutcome, RequestClass, RoutingError,
};
