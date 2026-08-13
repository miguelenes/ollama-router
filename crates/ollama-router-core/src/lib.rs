//! Shared routing, fleet, config, capacity, cloud, and job types.

pub mod capacity;
pub mod cloud;
pub mod config;
pub mod fleet;
pub mod jobs;
pub mod routing;

pub use cloud::{DemandScale, NoopDemandScale};
pub use config::{
    load_config, load_config_from, parse_nodes_env, parse_yaml, ConfigError, EnvSource, OsEnv,
    RouterConfig,
};
pub use fleet::{
    FleetState, FleetStateError, NodeId, NodeSnapshot, PressureLevel, Registry, RouterId,
    VerdaInstanceId, VerdaNodePersist, DEFAULT_STATE_PATH,
};
pub use jobs::{JobOutcome, JobStatus, ModelOrchestrator, OrchestratorError, StubOrchestrator};
pub use routing::{
    classify, parse_model_size_b, rank_nodes, RankOutcome, RequestClass, RoutingError,
};
