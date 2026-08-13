//! Idle reconcile (`CloudFleetManager`, Verda-only).
//!
//! Env permanent hosts are never destroyed. Verda teardown uses `delete_permanently`.
//! Demand scale-up is fire-and-forget from the proxy; this module owns the trait
//! and idle-candidate selection.

use std::time::{Duration, Instant};

use crate::fleet::{NodeId, NodeOrigin, NodeSnapshot, VerdaInstanceId};
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

/// One Verda node eligible for idle teardown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdleCandidate {
    pub node_id: NodeId,
    pub instance_id: VerdaInstanceId,
}

/// Inputs for idle eligibility (pure; no I/O).
#[derive(Clone, Copy, Debug)]
pub struct IdlePolicy {
    pub idle_timeout: Duration,
    pub grace_after_create: Duration,
    pub min_instances: u32,
}

/// Activity + origin facts needed to decide idle teardown.
#[derive(Clone, Debug)]
pub struct IdleNodeView {
    pub node_id: NodeId,
    pub instance_id: VerdaInstanceId,
    pub origin: NodeOrigin,
    pub inflight: u32,
    pub registered_at: Instant,
    pub last_client_request_at: Option<Instant>,
}

impl IdleNodeView {
    /// From a live snapshot plus FleetState instance id.
    pub fn from_snapshot(
        snap: &NodeSnapshot,
        instance_id: VerdaInstanceId,
        registered_at: Instant,
        last_client_request_at: Option<Instant>,
    ) -> Self {
        Self {
            node_id: snap.id.clone(),
            instance_id,
            origin: snap.origin,
            inflight: snap.inflight,
            registered_at,
            last_client_request_at,
        }
    }

    fn activity_anchor(&self) -> Instant {
        self.last_client_request_at.unwrap_or(self.registered_at)
    }
}

/// Owned Verda spots that may be destroyed, longest-idle first.
///
/// Never returns fleet.yaml (`Permanent`) hosts. Never scales below `min_instances`
/// of the in-flight set. Failed destroy must retain FleetState (caller).
pub fn idle_scale_down_candidates(
    in_flight: &[IdleNodeView],
    now: Instant,
    policy: IdlePolicy,
) -> Vec<IdleCandidate> {
    if in_flight.is_empty() {
        return Vec::new();
    }
    let mut idle: Vec<(&IdleNodeView, Instant)> = in_flight
        .iter()
        .filter(|node| node.origin == NodeOrigin::Verda)
        .filter(|node| node.inflight == 0)
        .filter(|node| {
            now.saturating_duration_since(node.registered_at) >= policy.grace_after_create
        })
        .filter(|node| now.saturating_duration_since(node.activity_anchor()) >= policy.idle_timeout)
        .map(|node| (node, node.activity_anchor()))
        .collect();
    idle.sort_by_key(|(_, anchor)| *anchor);
    let max_destroy = in_flight
        .len()
        .saturating_sub(policy.min_instances as usize);
    idle.into_iter()
        .take(max_destroy)
        .map(|(node, _)| IdleCandidate {
            node_id: node.node_id.clone(),
            instance_id: node.instance_id.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(
        id: &str,
        origin: NodeOrigin,
        inflight: u32,
        registered: Instant,
        last: Option<Instant>,
    ) -> IdleNodeView {
        IdleNodeView {
            node_id: NodeId::parse(id).unwrap(),
            instance_id: VerdaInstanceId::parse(id.trim_start_matches("verda-"))
                .unwrap_or_else(|_| VerdaInstanceId::parse("inst").unwrap()),
            origin,
            inflight,
            registered_at: registered,
            last_client_request_at: last,
        }
    }

    #[test]
    fn never_destroys_permanent_hosts() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(10_000);
        let nodes = [view("local", NodeOrigin::Permanent, 0, t0, None)];
        let policy = IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
        };
        assert!(idle_scale_down_candidates(&nodes, now, policy).is_empty());
    }

    #[test]
    fn respects_min_instances_and_inflight() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(10_000);
        let nodes = [
            view("verda-a", NodeOrigin::Verda, 0, t0, None),
            view("verda-b", NodeOrigin::Verda, 1, t0, None),
            view("verda-c", NodeOrigin::Verda, 0, t0, None),
        ];
        let policy = IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances: 1,
        };
        let out = idle_scale_down_candidates(&nodes, now, policy);
        // in_flight len=3, min=1 → max destroy 2, but inflight node is ineligible → 2 idle
        // wait: verda-b has inflight=1 so filtered. 2 idle, max_destroy=2, return 2.
        // min is against in_flight total (3), max_destroy=2, 2 idle eligible → 2.
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn grace_blocks_fresh_nodes() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(60);
        let nodes = [view("verda-a", NodeOrigin::Verda, 0, t0, None)];
        let policy = IdlePolicy {
            idle_timeout: Duration::from_secs(1),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
        };
        assert!(idle_scale_down_candidates(&nodes, now, policy).is_empty());
    }
}
