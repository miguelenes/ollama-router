//! Idle reconcile and demand scale-up (cloud providers).
//!
//! fleet.yaml permanent hosts are never destroyed. Cloud teardown uses
//! permanent delete/terminate. Demand scale-up is fire-and-forget from the
//! proxy; this module owns the trait and idle-candidate selection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::fleet::{CloudInstanceId, NodeId, NodeOrigin, NodeSnapshot};
use crate::routing::RoutingError;

/// Kick coalesced async capacity create. Must never block the client.
pub trait DemandScale: Send + Sync {
    fn request_scale_up(&self, reason: RoutingError);
}

/// Cloud lifecycle counters. Implemented in the binary (`Metrics`); this crate
/// stays free of prometheus.
pub trait FleetEvents: Send + Sync {
    fn cloud_event(&self, provider: &'static str, event: &'static str);
}

/// No-op when the binary has not attached metrics.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopFleetEvents;

impl FleetEvents for NoopFleetEvents {
    fn cloud_event(&self, _provider: &'static str, _event: &'static str) {}
}

/// No-op when no cloud provider is enabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopDemandScale;

impl DemandScale for NoopDemandScale {
    fn request_scale_up(&self, _reason: RoutingError) {}
}

/// One cloud node eligible for idle teardown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdleCandidate {
    pub node_id: NodeId,
    pub instance_id: CloudInstanceId,
}

/// Inputs for idle eligibility (pure; no I/O).
#[derive(Clone, Copy, Debug)]
pub struct IdlePolicy {
    pub idle_timeout: Duration,
    pub grace_after_create: Duration,
    pub min_instances: u32,
    /// Provider billing floor: younger instances are never idle candidates.
    pub min_lifetime: Duration,
}

/// Activity + origin facts needed to decide idle teardown.
#[derive(Clone, Debug)]
pub struct IdleNodeView {
    pub node_id: NodeId,
    pub instance_id: CloudInstanceId,
    pub origin: NodeOrigin,
    pub inflight: u32,
    pub registered_at: Instant,
    pub last_client_request_at: Option<Instant>,
}

impl IdleNodeView {
    /// From a live snapshot plus FleetState instance id.
    pub fn from_snapshot(
        snap: &NodeSnapshot,
        instance_id: CloudInstanceId,
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

    fn idle_eligible(&self, now: Instant, policy: IdlePolicy, provider: NodeOrigin) -> bool {
        self.origin == provider
            && self.inflight == 0
            && now.saturating_duration_since(self.registered_at) >= policy.grace_after_create
            && now.saturating_duration_since(self.registered_at) >= policy.min_lifetime
            && now.saturating_duration_since(self.activity_anchor()) >= policy.idle_timeout
    }
}

/// Owned cloud spots that may be destroyed, longest-idle first.
///
/// Never returns fleet.yaml (`Permanent`) hosts. Never scales below `min_instances`
/// of the in-flight set. Failed destroy must retain FleetState (caller).
/// Only nodes matching `provider` are candidates.
pub fn idle_scale_down_candidates(
    in_flight: &[IdleNodeView],
    now: Instant,
    policy: IdlePolicy,
    provider: NodeOrigin,
) -> Vec<IdleCandidate> {
    if in_flight.is_empty() {
        return Vec::new();
    }
    let mut idle: Vec<(&IdleNodeView, Instant)> = in_flight
        .iter()
        .filter(|node| node.idle_eligible(now, policy, provider))
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

/// Cloud spots to consider when trimming above `auto_scale_max_instances`.
///
/// Lowest activity (`last_client_request_at` else `registered_at`) first, then
/// lowest inflight. Never includes fleet.yaml (`Permanent`) hosts. Only nodes
/// matching `provider` are candidates. The caller drain-and-verifies and must
/// not go below `auto_scale_min_instances`.
pub fn excess_scale_down_order(
    in_flight: &[IdleNodeView],
    provider: NodeOrigin,
) -> Vec<IdleCandidate> {
    let mut ranked: Vec<&IdleNodeView> = in_flight
        .iter()
        .filter(|node| node.origin == provider)
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

/// Owned cloud instance ids that are billed but missing from FleetState and the
/// live registry, after `grace` has elapsed since `first_seen`.
///
/// Callers must pass only `is_owned` instance ids. Never include fleet.yaml
/// hosts. Destroy is the caller's job (permanent delete/terminate).
pub fn orphan_reclaim_candidates(
    owned: &[CloudInstanceId],
    fleet_instance_ids: &HashSet<String>,
    registry_cloud_instance_ids: &HashSet<String>,
    first_seen: &HashMap<String, Instant>,
    now: Instant,
    grace: Duration,
) -> Vec<CloudInstanceId> {
    let mut out = Vec::new();
    for id in owned {
        let key = id.as_str();
        if fleet_instance_ids.contains(key) {
            continue;
        }
        if registry_cloud_instance_ids.contains(key) {
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
/// Skip when process uptime is still inside create-grace so a boot loop cannot
/// mass-destroy.
pub fn should_destroy_on_shutdown(enabled: bool, uptime: Duration, grace: Duration) -> bool {
    enabled && uptime >= grace
}

/// Best currently eligible offer cached by a provider reconcile tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CachedOffer {
    /// Hourly price for the cap, metrics, and value-score ties.
    pub hourly_price: f64,
    /// Known VRAM GiB; `None` loses a value comparison against known VRAM.
    pub vram_gb: Option<f64>,
}

impl CachedOffer {
    /// Price per known VRAM GiB when VRAM is known and positive.
    pub fn value_score(&self) -> Option<f64> {
        let vram = self.vram_gb.filter(|v| v.is_finite() && *v > 0.0)?;
        if !self.hourly_price.is_finite() || self.hourly_price < 0.0 {
            return None;
        }
        Some(self.hourly_price / vram)
    }
}

/// Per-provider handle used by [`MultiProviderDemand`] (design D1).
pub trait CloudProviderHandle: Send + Sync {
    fn provider(&self) -> &'static str;
    fn request_scale_up(&self, reason: RoutingError);
    fn cached_best_offer(&self) -> Option<CachedOffer>;
    fn below_ceiling(&self) -> bool;
}

/// Fan proxy demand to the best-value enabled provider below its ceiling.
pub struct MultiProviderDemand {
    providers: Vec<std::sync::Arc<dyn CloudProviderHandle>>,
}

impl MultiProviderDemand {
    pub fn new(providers: Vec<std::sync::Arc<dyn CloudProviderHandle>>) -> Self {
        Self { providers }
    }

    /// Pick the best provider for a scale-up (pure ranking over cached offers).
    ///
    /// Eligible = `below_ceiling()` and has a cached offer. Rank by lowest
    /// price/known VRAM; unknown VRAM sorts last; equal scores break by lower
    /// hourly price, then provider name.
    pub fn pick_provider(&self) -> Option<&dyn CloudProviderHandle> {
        let mut ranked: Vec<(&dyn CloudProviderHandle, CachedOffer)> = self
            .providers
            .iter()
            .filter_map(|p| {
                if !p.below_ceiling() {
                    return None;
                }
                let offer = p.cached_best_offer()?;
                Some((p.as_ref(), offer))
            })
            .collect();
        ranked.sort_by(
            |(a, ao), (b, bo)| match (ao.value_score(), bo.value_score()) {
                (Some(sa), Some(sb)) => sa
                    .partial_cmp(&sb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(
                        ao.hourly_price
                            .partial_cmp(&bo.hourly_price)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                    .then(a.provider().cmp(b.provider())),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => ao
                    .hourly_price
                    .partial_cmp(&bo.hourly_price)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.provider().cmp(b.provider())),
            },
        );
        ranked.into_iter().next().map(|(p, _)| p)
    }
}

impl DemandScale for MultiProviderDemand {
    fn request_scale_up(&self, reason: RoutingError) {
        if let Some(provider) = self.pick_provider() {
            provider.request_scale_up(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn view(
        id: &str,
        origin: NodeOrigin,
        inflight: u32,
        registered: Instant,
        last: Option<Instant>,
    ) -> IdleNodeView {
        IdleNodeView {
            node_id: NodeId::parse(id).unwrap(),
            instance_id: CloudInstanceId::parse(id.trim_start_matches("verda-"))
                .unwrap_or_else(|_| CloudInstanceId::parse("inst").unwrap()),
            origin,
            inflight,
            registered_at: registered,
            last_client_request_at: last,
        }
    }

    fn policy(min_instances: u32) -> IdlePolicy {
        IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances,
            min_lifetime: Duration::ZERO,
        }
    }

    #[test]
    fn never_destroys_permanent_hosts() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(10_000);
        let nodes = [view("local", NodeOrigin::Permanent, 0, t0, None)];
        assert!(idle_scale_down_candidates(&nodes, now, policy(0), NodeOrigin::Verda).is_empty());
        assert!(
            excess_scale_down_order(&nodes, NodeOrigin::Verda).is_empty(),
            "fleet.yaml hosts are never destroyed"
        );
        assert!(
            excess_scale_down_order(&nodes, NodeOrigin::Runpod).is_empty(),
            "fleet.yaml hosts are never destroyed"
        );
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
        let out = idle_scale_down_candidates(&nodes, now, policy(1), NodeOrigin::Verda);
        // in_flight len=3, min=1 → max destroy 2, but inflight node is ineligible → 2 idle
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn grace_blocks_fresh_nodes() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(60);
        let nodes = [view("verda-a", NodeOrigin::Verda, 0, t0, None)];
        let p = IdlePolicy {
            idle_timeout: Duration::from_secs(1),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
            min_lifetime: Duration::ZERO,
        };
        assert!(idle_scale_down_candidates(&nodes, now, p, NodeOrigin::Verda).is_empty());
    }

    #[test]
    fn restart_without_client_activity_uses_registered_at() {
        let registered = Instant::now();
        let now = registered + Duration::from_secs(60);
        let nodes = [view("verda-a", NodeOrigin::Verda, 0, registered, None)];
        assert!(
            idle_scale_down_candidates(&nodes, now, policy(0), NodeOrigin::Verda).is_empty(),
            "registered_at fallback must not mass-destroy after a router restart"
        );
    }

    #[test]
    fn min_lifetime_is_honored() {
        let t0 = Instant::now();
        // Past idle_timeout and grace, but younger than min_lifetime.
        let now = t0 + Duration::from_secs(3_600);
        let nodes = [view("verda-a", NodeOrigin::Verda, 0, t0, Some(t0))];
        let p = IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
            min_lifetime: Duration::from_secs(7_200),
        };
        assert!(
            idle_scale_down_candidates(&nodes, now, p, NodeOrigin::Verda).is_empty(),
            "Minimum lifetime is honored"
        );
    }

    #[test]
    fn fine_grained_billing_tears_down_promptly() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(10_000);
        let nodes = [view("runpod-a", NodeOrigin::Runpod, 0, t0, Some(t0))];
        let p = IdlePolicy {
            idle_timeout: Duration::from_secs(900),
            grace_after_create: Duration::from_secs(300),
            min_instances: 0,
            min_lifetime: Duration::ZERO,
        };
        let out = idle_scale_down_candidates(&nodes, now, p, NodeOrigin::Runpod);
        assert_eq!(out.len(), 1, "Fine-grained billing tears down promptly");
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

    fn iid(id: &str) -> CloudInstanceId {
        CloudInstanceId::parse(id).unwrap()
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
        let out = excess_scale_down_order(&nodes, NodeOrigin::Verda);
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

    struct FakeHandle {
        name: &'static str,
        offer: Option<CachedOffer>,
        below_ceiling: bool,
        hits: Mutex<u32>,
    }

    impl CloudProviderHandle for FakeHandle {
        fn provider(&self) -> &'static str {
            self.name
        }

        fn request_scale_up(&self, _reason: RoutingError) {
            *self.hits.lock().unwrap() += 1;
        }

        fn cached_best_offer(&self) -> Option<CachedOffer> {
            self.offer
        }

        fn below_ceiling(&self) -> bool {
            self.below_ceiling
        }
    }

    #[test]
    fn better_value_provider_wins() {
        let a = Arc::new(FakeHandle {
            name: "verda",
            offer: Some(CachedOffer {
                hourly_price: 0.40,
                vram_gb: Some(48.0),
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let b = Arc::new(FakeHandle {
            name: "runpod",
            offer: Some(CachedOffer {
                hourly_price: 0.20,
                vram_gb: Some(8.0),
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let demand = MultiProviderDemand::new(vec![b.clone(), a.clone()]);
        assert_eq!(demand.pick_provider().unwrap().provider(), "verda");
        demand.request_scale_up(RoutingError::Capacity);
        assert_eq!(*a.hits.lock().unwrap(), 1);
        assert_eq!(*b.hits.lock().unwrap(), 0);
    }

    #[test]
    fn stockout_falls_back() {
        let stockout = Arc::new(FakeHandle {
            name: "verda",
            offer: None,
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let fallback = Arc::new(FakeHandle {
            name: "runpod",
            offer: Some(CachedOffer {
                hourly_price: 0.50,
                vram_gb: Some(24.0),
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let demand = MultiProviderDemand::new(vec![stockout.clone(), fallback.clone()]);
        assert_eq!(demand.pick_provider().unwrap().provider(), "runpod");
        demand.request_scale_up(RoutingError::NoHealthy);
        assert_eq!(*fallback.hits.lock().unwrap(), 1);
        assert_eq!(*stockout.hits.lock().unwrap(), 0);
    }

    #[test]
    fn unknown_vram_loses_cross_provider_value_comparison() {
        let known = Arc::new(FakeHandle {
            name: "verda",
            offer: Some(CachedOffer {
                hourly_price: 1.0,
                vram_gb: Some(24.0),
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let unknown = Arc::new(FakeHandle {
            name: "runpod",
            offer: Some(CachedOffer {
                hourly_price: 0.10,
                vram_gb: None,
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let demand = MultiProviderDemand::new(vec![unknown, known]);
        assert_eq!(demand.pick_provider().unwrap().provider(), "verda");
    }

    #[test]
    fn ceiling_skips_provider_without_aborting() {
        let at_cap = Arc::new(FakeHandle {
            name: "verda",
            offer: Some(CachedOffer {
                hourly_price: 0.10,
                vram_gb: Some(80.0),
            }),
            below_ceiling: false,
            hits: Mutex::new(0),
        });
        let open = Arc::new(FakeHandle {
            name: "runpod",
            offer: Some(CachedOffer {
                hourly_price: 0.50,
                vram_gb: Some(24.0),
            }),
            below_ceiling: true,
            hits: Mutex::new(0),
        });
        let demand = MultiProviderDemand::new(vec![at_cap.clone(), open.clone()]);
        assert_eq!(demand.pick_provider().unwrap().provider(), "runpod");
        demand.request_scale_up(RoutingError::Saturated);
        assert_eq!(*open.hits.lock().unwrap(), 1);
        assert_eq!(*at_cap.hits.lock().unwrap(), 0);
    }
}
