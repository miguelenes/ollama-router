//! Idle reconcile (`CloudFleetManager`, Verda-only) — later slice.
//!
//! Env permanent hosts are never destroyed. Verda teardown uses `delete_permanently`.
//! Demand scale-up is fire-and-forget from the proxy; this module owns the trait.

use crate::routing::RoutingError;

/// Kick coalesced async capacity create. Must never block the client.
pub trait DemandScale: Send + Sync {
    fn request_scale_up(&self, reason: RoutingError);
}

/// No-op until the Verda manager lands.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopDemandScale;

impl DemandScale for NoopDemandScale {
    fn request_scale_up(&self, _reason: RoutingError) {}
}
