//! Shared routing, fleet, config, capacity, cloud, and job types.

pub mod capacity;
pub mod cloud;
pub mod config;
pub mod fleet;
pub mod jobs;
pub mod provision;
pub mod routing;

pub use capacity::{
    bytes_to_gib, capacity_target, merge_capacity, CapacityClient, CapacityError, CapacityInventory,
    CapacityProbe, CapacityReport, CapacitySource, CapacityTarget, MergeOutcome, Pressure,
    PressureEnvelope, BYTES_PER_GIB,
};
pub use cloud::{
    idle_scale_down_candidates, should_destroy_on_shutdown, DemandScale, FleetEvents,
    IdleCandidate, IdleNodeView, IdlePolicy, NoopDemandScale, NoopFleetEvents,
};
pub use config::{
    hydrate_node_urls, load_config, load_config_from, parse_yaml, ConfigError, EnvSource, OsEnv,
    RouterConfig,
};
pub use fleet::{
    load_fleet_nodes, parse_fleet_yaml, url_host_is_public_ipv4, FleetState, FleetStateError,
    NodeId, NodeOrigin, NodeSnapshot, PressureLevel, Registry, RouterId, VerdaInstanceId,
    VerdaNodePersist, DEFAULT_FLEET_PATH, DEFAULT_STATE_PATH,
};
pub use jobs::{
    Job, JobId, JobKind, JobObserver, JobOutcome, JobStatus, JobStore, JobTarget,
    ModelOrchestrator, OrchestratorError, PullOrchestrator, StubOrchestrator, TargetStatus,
};
pub use provision::{
    posix_quote, provision_config_from_defaults, read_provision_script, redact_authkey,
    resolve_provision_script, NodeProvisioner, ProvisionFuture, ProvisionOpts, ProvisionPhase,
    ProvisionResult, ProvisionStatus, DEFAULT_SCRIPT_PATH, REMOTE_SCRIPT,
};
pub use routing::{
    classify, parse_model_size_b, placement_class, placement_eligible_node_ids,
    ram_pressure_blocks_placement, rank_nodes, resolve_target_nodes, PlacementError, RankOutcome,
    RequestClass, RoutingError, TargetSpec, TARGET_ALL,
};
