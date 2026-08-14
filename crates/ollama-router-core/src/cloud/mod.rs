//! Idle reconcile and demand scale-up (Verda-only).
//!
//! fleet.yaml permanent hosts are never destroyed. Verda teardown uses
//! `delete_permanently`. Demand scale-up is fire-and-forget from the proxy;
//! this module owns the trait and idle-candidate selection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::fleet::{NodeId, NodeOrigin, NodeSnapshot, VerdaInstanceId};
use crate::routing::RoutingError;

/// Kick coalesced async capacity create. Must never block the client.
pub trait DemandScale: Send + Sync {
    fn request_scale_up(&self, reason: RoutingError);
}

/// Verda lifecycle counters. Implemented in the binary (`Metrics`); this crate
/// stays free of prometheus.
pub trait FleetEvents: Send + Sync {
    fn verda_event(&self, event: &'static str);
}

/// No-op when the binary has not attached metrics.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopFleetEvents;

impl FleetEvents for NoopFleetEvents {
    fn verda_event(&self, _event: &'static str) {}
}

/// No-op when Verda is disabled.
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

    fn idle_eligible(&self, now: Instant, policy: IdlePolicy) -> bool {
        self.origin == NodeOrigin::Verda
            && self.inflight == 0
            && now.saturating_duration_since(self.registered_at) >= policy.grace_after_create
            && now.saturating_duration_since(self.activity_anchor()) >= policy.idle_timeout
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
        .filter(|node| node.idle_eligible(now, policy))
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

/// Verda spots to consider when trimming above `auto_scale_max_instances`.
///
/// Lowest activity (`last_client_request_at` else `registered_at`) first, then
/// lowest inflight. Never includes fleet.yaml (`Permanent`) hosts. The caller
/// drain-and-verifies and must not go below `auto_scale_min_instances`.
pub fn excess_scale_down_order(in_flight: &[IdleNodeView]) -> Vec<IdleCandidate> {
    let mut ranked: Vec<&IdleNodeView> = in_flight
        .iter()
        .filter(|node| node.origin == NodeOrigin::Verda)
        .collect();
    ranked.sort_by(|a, b| {
        a.activity_anchor()
            .cmp(&b.activity_anchor())
            .then(a.inflight.cmp(&b.inflight))
            .then(a.node_id.as_str().cmp(b.node_id.as_str()))
    });
    ranked
        .into_iter()
        .map(|node| IdleCandidate {
            node_id: node.node_id.clone(),
            instance_id: node.instance_id.clone(),
        })
        .collect()
}

/// Owned Verda instance ids that are billed but missing from FleetState and the
/// live registry, after `grace` has elapsed since `first_seen`.
///
/// Callers must pass only `is_owned` instance ids. Never include fleet.yaml
/// hosts. Destroy is the caller's job (`delete_permanently`).
pub fn orphan_reclaim_candidates(
    owned: &[VerdaInstanceId],
    fleet_instance_ids: &HashSet<String>,
    registry_verda_instance_ids: &HashSet<String>,
    first_seen: &HashMap<String, Instant>,
    now: Instant,
    grace: Duration,
) -> Vec<VerdaInstanceId> {
    let mut out = Vec::new();
    for id in owned {
        let key = id.as_str();
        if fleet_instance_ids.contains(key) {
            continue;
        }
        if registry_verda_instance_ids.contains(key) {
            continue;
        }
        let Some(seen) = first_seen.get(key) else {
            continue;
        };
        if now.saturating_duration_since(*seen) < grace {
            continue;
        }
        out.push(id.clone());
    }
    out
}

/// Crash-loop guard for `destroy_on_shutdown`.
///
/// Python always deleted owned spots on SIGTERM. Skip when process uptime is
/// still inside create-grace so a boot loop cannot mass-destroy.
pub fn should_destroy_on_shutdown(enabled: bool, uptime: Duration, grace: Duration) -> bool {
    enabled && uptime >= grace
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

    #[test]
    fn restart_without_client_activity_uses_registered_at() {
        let registered = Instant::now();
        let now = registered + Duration::from_secs(60);
        let nodes = [view("verda-a", NodeOrigin::Verda, 0, registered, None)];
        let policy = IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
        };
        assert!(
            idle_scale_down_candidates(&nodes, now, policy).is_empty(),
            "registered_at fallback must not mass-destroy after a router restart"
        );
    }

    #[test]
    fn should_destroy_on_shutdown_skips_inside_grace() {
        let grace = Duration::from_secs(300);
        assert!(!should_destroy_on_shutdown(
            true,
            Duration::from_secs(10),
            grace
        ));
        assert!(should_destroy_on_shutdown(
            true,
            Duration::from_secs(300),
            grace
        ));
        assert!(!should_destroy_on_shutdown(
            false,
            Duration::from_secs(10_000),
            grace
        ));
    }

    fn iid(id: &str) -> VerdaInstanceId {
        VerdaInstanceId::parse(id).unwrap()
    }

    #[test]
    fn orphan_reclaim_skips_fleetstate_registry_and_grace() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(2_000);
        let owned = [iid("orphan"), iid("in-fleet"), iid("in-reg"), iid("fresh")];
        let fleet = HashSet::from(["in-fleet".to_string()]);
        let registry = HashSet::from(["in-reg".to_string()]);
        let first_seen = HashMap::from([
            ("orphan".to_string(), t0),
            ("in-fleet".to_string(), t0),
            ("in-reg".to_string(), t0),
            ("fresh".to_string(), now),
        ]);
        let out = orphan_reclaim_candidates(
            &owned,
            &fleet,
            &registry,
            &first_seen,
            now,
            Duration::from_secs(1_800),
        );
        assert_eq!(out, vec![iid("orphan")]);
    }

    #[test]
    fn excess_order_lowest_activity_then_inflight_never_permanent() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(10);
        let t2 = t0 + Duration::from_secs(20);
        let nodes = [
            view("verda-busy", NodeOrigin::Verda, 1, t0, Some(t2)),
            view("local", NodeOrigin::Permanent, 0, t0, None),
            view("verda-old", NodeOrigin::Verda, 0, t0, Some(t0)),
            view("verda-mid", NodeOrigin::Verda, 0, t0, Some(t1)),
        ];
        let out = excess_scale_down_order(&nodes);
        let ids: Vec<&str> = out.iter().map(|c| c.node_id.as_str()).collect();
        assert_eq!(ids, ["verda-old", "verda-mid", "verda-busy"]);
    }

    #[test]
    fn orphan_reclaim_never_returns_without_first_seen() {
        let now = Instant::now();
        let owned = [iid("ghost")];
        let out = orphan_reclaim_candidates(
            &owned,
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
            now,
            Duration::from_secs(0),
        );
        assert!(out.is_empty());
    }
}
