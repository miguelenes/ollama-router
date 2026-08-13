//! Registry, env inventory, durable FleetState.

pub(crate) mod env;
pub(crate) mod ids;
pub(crate) mod state;
pub(crate) mod tailscale;

pub use env::parse_host_environ;
pub use ids::{NodeId, RouterId, VerdaInstanceId};
pub use state::{
    FleetState, FleetStateEntry, FleetStateError, VerdaNodePersist, DEFAULT_STATE_PATH,
};
pub use tailscale::{
    is_tailscale_ipv4, ollama_url_for_tailscale_ip, routing_url_from_fields, url_host_is_ip,
    url_host_is_tailscale,
};
