//! Registry, declarative fleet file, durable FleetState.

pub(crate) mod file;
pub(crate) mod ids;
pub(crate) mod registry;
pub(crate) mod state;
pub(crate) mod tailscale;

pub use file::{fleet_path_from_env, load_fleet_nodes, parse_fleet_yaml, DEFAULT_FLEET_PATH};
pub use ids::{NodeId, RouterId, VerdaInstanceId};
pub use registry::{
    model_base, normalize_model, suggested_max_inflight, NodeOrigin, NodeSnapshot, PressureLevel,
    Registry,
};
pub use state::{
    FleetState, FleetStateEntry, FleetStateError, VerdaNodePersist, DEFAULT_STATE_PATH,
};
pub use tailscale::{
    is_tailscale_ipv4, ollama_url_for_tailscale_ip, routing_url_from_fields, url_host_is_ip,
    url_host_is_tailscale,
};
