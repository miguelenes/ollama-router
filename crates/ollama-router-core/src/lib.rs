//! Shared routing, fleet, config, capacity, cloud, and job types.

pub mod capacity;
pub mod cloud;
pub mod config;
pub mod fleet;
pub mod http_util;
pub mod jobs;
pub mod routing;

pub use capacity::{
    bytes_to_gib, capacity_target, merge_capacity, CapacityClient, CapacityError,
    CapacityInventory, CapacityProbe, CapacityReport, CapacitySource, CapacityTarget, GpuBackend,
    GpuDetail, MergeOutcome, Pressure, PressureEnvelope, BYTES_PER_GIB,
};
pub use cloud::{
    excess_scale_down_order, idle_scale_down_candidates, orphan_reclaim_candidates,
    should_destroy_on_shutdown, DemandScale, FleetEvents, IdleCandidate, IdleNodeView, IdlePolicy,
    NoopDemandScale, NoopFleetEvents,
};
pub use config::{
    http_url_for_bind, hydrate_node_urls, load_config, load_config_from, parse_yaml,
    socket_addr_for_bind, ConfigError, EnvSource, OsEnv, RouterConfig, TunnelConfig,
};
pub use fleet::{
    load_fleet_nodes, parse_fleet_yaml, routing_url_blocked_reason, share_id_looks_public,
    url_host_is_public_ip, url_host_is_public_ipv4, url_host_is_public_share, EnrollPersist,
    FleetState, FleetStateError, GpuSnapshot, InflightAdmit, NodeId, NodeOrigin, NodeSnapshot,
    PressureLevel, Registry, RouterId, VerdaInstanceId, VerdaNodePersist, DEFAULT_FLEET_PATH,
    DEFAULT_STATE_PATH,
};
pub use http_util::{read_reqwest_capped, reqwest_error_for_log, ProbeBodyError};
pub use jobs::{
    Job, JobId, JobKind, JobObserver, JobOutcome, JobStatus, JobStore, JobTarget,
    ModelOrchestrator, OrchestratorError, PullOrchestrator, StubOrchestrator, TargetStatus,
};
pub use routing::{
    classify, parse_model_size_b, placement_class, placement_eligible_node_ids,
    ram_pressure_blocks_placement, rank_nodes, resolve_target_nodes, PlacementError, RankOutcome,
    RequestClass, RoutingError, TargetSpec, TARGET_ALL,
};
