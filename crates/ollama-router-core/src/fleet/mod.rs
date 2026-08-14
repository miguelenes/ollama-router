//! Registry, declarative fleet file, durable FleetState.

pub(crate) mod file;
pub(crate) mod ids;
pub(crate) mod registry;
pub(crate) mod state;
pub(crate) mod url_policy;

pub use file::{fleet_path_from_env, load_fleet_nodes, parse_fleet_yaml, DEFAULT_FLEET_PATH};
pub use ids::{NodeId, RouterId, VerdaInstanceId};
pub use registry::{
    model_base, normalize_model, suggested_max_inflight, GpuSnapshot, NodeOrigin, NodeSnapshot,
    PressureLevel, Registry,
};
pub use state::{
    EnrollPersist, FleetState, FleetStateEntry, FleetStateError, VerdaNodePersist,
    DEFAULT_STATE_PATH,
};
pub use url_policy::{
    routing_url_blocked_reason, share_id_looks_public, url_host_is_ip, url_host_is_loopback,
    url_host_is_public_ip, url_host_is_public_ipv4, url_host_is_public_share, url_host_is_rfc1918,
    url_is_safe_overlay,
};
