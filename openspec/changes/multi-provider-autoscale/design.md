# Design — Multi-Provider Autoscale (Verda + RunPod)

## Context

See `proposal.md` — Why. Current state that shapes the approach (verified in-tree):

- `crates/ollama-router-core/src/cloud/mod.rs` is pure policy: `DemandScale` (sync fire-and-forget), `FleetEvents::verda_event`, `IdlePolicy` / `IdleNodeView` / `idle_scale_down_candidates` / `excess_scale_down_order` / `orphan_reclaim_candidates`. It is Verda-typed (`VerdaInstanceId`, `origin == NodeOrigin::Verda`).
- `VerdaManager` (`crates/ollama-router-verda/src/manager.rs`) owns everything stateful: OAuth client, `ensure` (adopt-first), coalesced `create_additional`, `run_reconcile_loop()`, idle/excess teardown execution, orphan reclaim, `destroy_on_shutdown`. It implements `DemandScale`; `main.rs` wires `state.demand = Arc::new(mgr.clone())` and `supervisor.spawn(mgr.run_reconcile_loop())`.
- The proxy calls `state.demand.request_scale_up(reason)` on `NoHealthy` / `Saturated` / `Capacity` and immediately returns 503 + Retry-After.
- `NodeOrigin { Permanent, Verda }` (`fleet/registry.rs`); FleetState rows carry `managed_by: Option<String>` (`"verda"`) plus `verda_instance_id/location/instance_type/os_volume_id/spot_price_per_hour` columns; enroll `origin=verda` updates only existing `managed_by=verda` rows (`http/admin.rs`).
- Metrics: `ollama_router_verda_instances`, `ollama_router_verda_spot_price_per_hour`, `ollama_router_verda_events_total{event}`, `ollama_router_tunnel_up{node}` (`http/metrics.rs`); compose Grafana fleet dashboard consumes them.
- Admin routes `/router/v1/verda/{status,ensure,destroy}` exist; `tests/admin.rs::no_thunder_or_runpod_admin_paths` currently asserts the RunPod path is absent.
- RunPod facts (from current RunPod docs): pod lifecycle on REST v1 (`https://rest.runpod.io/v1` — `POST /pods` with `interruptible: true`, ordered `gpuTypeIds` + `gpuTypePriority`, `cloudType SECURE|COMMUNITY`, `imageName`, `dockerStartCmd`, `env`, `containerDiskInGb`, `volumeInGb`; `GET /pods`, `GET /pods/{id}`, `DELETE /pods/{id}`). GPU price + availability catalog only on REST v2 beta (`https://api.runpod.io/v2/catalog/gpus?include=AVAILABILITY&cloud=...` → `memory` (VRAM GB), `price.secure/community`, availability `NONE..HIGH`, per-datacenter). Compute bills per second; stopped pods keep billing volume storage; no minimum billing period (account needs 1 h of credits to deploy). Spot prices are not exposed in the v2 catalog; the created pod reports actual `costPerHr` and `gpu.communitySpotPrice`/`secureSpotPrice`. RunPod has no instance tags. 429s come with `RateLimit` headers.

## Goals / Non-Goals

**Goals:**

- One provider seam in core so the proxy and reconcile policy stop being Verda-specific, with Verda behavior byte-for-byte preserved where the specs don't change it.
- A `crates/ollama-router-runpod` crate structurally mirroring `ollama-router-verda` (client / selector / manager / startup / tests) so the test and review pattern carries over.
- Cost policy: interruptible-first, per-provider hourly cap enforced, best value-for-money (lowest $/known VRAM GiB) as the default for every provider, billing-aware teardown timing.

**Non-Goals (design-level):**

- No central "one loop drives all providers" rewrite — each manager keeps its own reconcile loop (verify-first; the Verda loop is battle-tested).
- No generic plugin/provider registry; exactly two concrete providers, compile-time wired.
- No RunPod GraphQL usage and no new TLS/transport dependencies.
- No changes to `rank_nodes`, request classification, or the job orchestrator.

## Decisions

### D1: Coordinator over trait-object manager

Keep `VerdaManager` and the new `RunpodManager` as independent, self-reconciling managers. Add a small core trait (in `cloud/`) implemented by both:

```rust
pub struct CachedOffer {
    pub hourly_price: f64,       // for the cap, metrics, and value-score ties
    pub vram_gb: Option<f64>,    // known VRAM; None loses a value comparison
}

pub trait CloudProviderHandle: Send + Sync {
    fn provider(&self) -> &'static str;              // "verda" | "runpod"
    fn request_scale_up(&self, reason: RoutingError); // coalesced, non-blocking
    fn cached_best_offer(&self) -> Option<CachedOffer>; // refreshed by own reconcile tick
    fn below_ceiling(&self) -> bool;                  // cached owned-count vs max_instances
}
```

A `MultiProviderDemand` (implements existing `DemandScale`) fans the proxy's `request_scale_up` into the enabled provider that is `below_ceiling()` with the best cached value score (lowest `hourly_price / known vram_gb`; unknown VRAM sorts last; equal scores break by lower hourly price), falling back to the next. `main.rs` wires it in place of the direct Verda handle; with one provider enabled it degenerates to today's behavior.

*Why:* the 503 hot path must stay sync and non-blocking, so cross-provider comparison must read cached values, not call catalogs. Comparing raw hourly price would pick a cheap tiny GPU over a better-value larger one and fight the shared selection spec. Each manager already refreshes provider state on its poll interval — piggyback the best eligible offer there.
*Alternative rejected:* one async `CloudProvider` mega-trait (create/destroy/list driven by a central core loop). It would force moving working Verda code wholesale behind boxed async traits, a large-risk refactor the specs don't require.

### D2: Origin and id generalization

`NodeOrigin` gains a `Runpod` variant (label `"runpod"` for `node_info`/CLI). `VerdaInstanceId` is renamed to `CloudInstanceId` (same validation; it already just wraps a provider instance id) — mechanical rename, no compatibility alias. The pure policy fns take the provider's origin as a parameter (`idle_eligible` matches "this provider's origin" instead of hardcoded `Verda`), and `IdlePolicy` gains `min_lifetime: Duration` (D6). `orphan_reclaim_candidates` and `excess_scale_down_order` are already id/origin-generic after that parameterization.

*Alternative rejected:* `NodeOrigin::Cloud { provider }` — churns every match site and serde representation for no behavioral gain with two providers.

### D3: FleetState stays additive

Keep the existing `verda_*` columns untouched; add optional `runpod_pod_id`, `runpod_gpu_type`, `runpod_data_center`, `runpod_cost_per_hour`, and `managed_by = "runpod"`. Add `list_runpod_nodes` / extend the snapshot helpers by `managed_by` value. Enroll gains `origin=runpod` handling that mirrors the `verda_not_owned` conflict rule.

*Why:* existing on-disk state files keep loading with zero migration; serde already skips `None` columns.
*Alternative rejected:* provider-neutral `cloud_instance_id` columns — requires migrating live state files for cosmetic benefit.

### D4: RunPod API split — v1 lifecycle, v2 catalog

Client (`crates/ollama-router-runpod/src/client.rs`, reqwest rustls, bearer `RUNPOD_API_KEY`):

- Lifecycle on REST v1: `POST /pods` (create, `interruptible` from config, `gpuTypePriority: "custom"` with our ranked `gpuTypeIds`), `GET /pods` (list, name-filtered), `GET /pods/{id}` (status poll), `DELETE /pods/{id}` (terminate permanently).
- Discovery on REST v2 beta: `GET /v2/catalog/gpus?include=AVAILABILITY&cloud=<type>` for VRAM, on-demand price, and availability.
- All DTOs `serde` ignore-unknown. 429 handled with backoff honoring `Retry-After`/`RateLimit` headers. Catalog failure soft-fails the tick (no create, reason code) per the provider-isolation spec.

*Alternative rejected:* GraphQL API — legacy, spot pricing no longer documented, adds a query-building dependency.

### D5a: Shared value default; existing cap knobs; no third price field

Verda already has `SelectionStrategy::{Cheapest, BestValue}` and `max_spot_price_per_hour` in the selector. Flip the default in `router.defaults.yaml` and `VerdaConfig` from `cheapest` to `best_value`. `cheapest` remains an explicit YAML/env opt-in. RunPod has no strategy enum — it always ranks by price/VRAM (the shared default).

Hourly caps stay per-provider and keep shipped names:

- Verda: `max_spot_price_per_hour` (already used as the selector filter)
- RunPod: `max_price_per_hour`

`verda.demand_scale_price_per_hour` is already documented as a metrics/log fallback when FleetState has no persisted spot price. It is **not** a cap and MUST NOT be aliased onto `max_spot_price_per_hour`. No global cap.

A set cap MUST be `> 0` at config validate time (today `demand_scale_price_per_hour` only rejects `< 0`; apply the same `> 0` rule to both cap knobs).

*Alternative rejected:* introducing a third `max_price_per_hour` on Verda, or treating `demand_scale_price_per_hour` as the cap — both would confuse operators who already have the overlay example.

### D5: Selection and price-cap enforcement without spot quotes

`selector.rs`: filter catalog rows to available, VRAM within `min_vram_gb..=max_vram_gb`, allow/deny GPU-type lists, allowed data centers, and on-demand price ≤ `max_price_per_hour` (when set); rank ascending by price-per-VRAM-GiB; emit the ranked `gpuTypeIds` list so one create call lets RunPod fill the best available offer. Because the v2 catalog exposes no spot quotes, the cap is enforced conservatively against on-demand price pre-create (interruptible actual ≤ on-demand), then verified against the created pod's actual `costPerHr`: if the actual exceeds the cap, the pod is terminated immediately and the create counts as a stockout. Actual `costPerHr` lands in FleetState and the price metric.

*Alternative rejected:* estimating spot prices with a fixed discount factor — invented numbers; the verify-after-create step is exact.

### D6: Billing-aware teardown as a policy knob, not provider code

New per-provider `min_lifetime_seconds` (default 0) feeding `IdlePolicy.min_lifetime`; an instance younger than it is never an idle candidate. RunPod bills per second → default 0 (teardown = idle_timeout + grace, unchanged shape). Verda gets the same knob so operators can align teardown to that provider's billing period. This satisfies the cloud-autoscale billing-timing requirement with one pure-function change testable next to the module.

### D7: Bootstrap via stock image + dockerStartCmd (no custom image pipeline)

Reuse the Verda startup-script approach: the manager renders the same agent-bootstrap script (install node-agent package from GitHub release, zrok **private** share enable, agent `serve`, enroll against `enroll_url`) and passes it as `dockerStartCmd: ["bash","-lc", <script>]` on a configurable stock CUDA-capable `image`. `env` carries only names configured via `*_env` knobs (zrok enable token, enroll bearer) — memory-only, never logged. `ports: []` (nothing exposed; the RunPod proxy/public IP is never used), `volumeInGb: 0` (no persistent volume → nothing to bill after terminate), `containerDiskInGb` tunable. Ollama inside the pod listens on loopback; reachability is exclusively the zrok private share, preserving `public_url_blocked` semantics untouched.

*Alternative rejected:* building and publishing a dedicated node image — new CI surface and registry coupling; can be a later optimization since `image` is a tunable.

### D8: Interruption recovery inside the RunPod reconcile tick

Each tick lists managed pods; a managed pod not `RUNNING` (interrupted spot → `EXITED`, or `ERROR`, or missing) is terminated permanently and its FleetState row removed on success. Replacement follows the envelope: below `min_instances` → create on the same tick; otherwise wait for the demand path. Health probes already mark the node unhealthy in seconds, so routing correctness never depends on the tick.

### D9: Metrics go provider-labeled; Verda-named series are replaced

Replace `ollama_router_verda_instances` → `ollama_router_cloud_instances{provider}`, `ollama_router_verda_events_total{event}` → `ollama_router_cloud_events_total{provider,event}`, `ollama_router_verda_spot_price_per_hour` → `ollama_router_cloud_price_per_hour{provider}`. `FleetEvents::verda_event(event)` becomes `cloud_event(provider, event)`. `ollama_router_tunnel_up{node}` and `node_info{node,origin,role}` are unchanged (origin gains the `runpod` value). The compose Grafana fleet dashboard is updated in the same change (metric renames must not orphan panels). No model-name labels anywhere.

*Alternative rejected:* keeping verda-named series alongside new generic ones — two diverging metric families and permanent dashboard debt.

### D10: Admin surface mirrors Verda

Add `/router/v1/runpod/{status,ensure,destroy}` with the same fail-closed bearer semantics. `tests/admin.rs::no_thunder_or_runpod_admin_paths` keeps only its Thunder half; the config tests asserting `runpod:` is an unknown field flip into "runpod overlay parses / thunder still rejected".

## Risks / Trade-offs

- [v2 catalog is beta and may change shape] → ignore-unknown DTOs touch only five fields (`id`, `memory`, `price`, availability, data centers); catalog failure soft-fails the tick with a reason code and the provider is skipped by `MultiProviderDemand` (no cached offer).
- [Spot price unknown pre-create] → conservative cap on on-demand price plus post-create verify-and-terminate (D5); worst case is seconds of billed time on an over-cap pod.
- [Name-scheme ownership is weaker than Verda tags] → pod names embed `managed_by` marker and router id; manage only pods matching the scheme *and* reconciled against FleetState rows; foreign pods are never touched (spec scenario + httpmock test).
- [Image pull can make pod boot slow (minutes)] → `grace_after_create` and `create_timeout_seconds` knobs mirror Verda; recommend a slim default image in docs.
- [Stopped (interrupted) pods keep billing storage until terminated] → interruption cleanup is part of every reconcile tick, and teardown always terminates (never stops).
- [Metric renames break existing dashboards] → dashboard JSON is updated in the same change; the repo rule "metric names change ⇒ update dashboards" makes this the sanctioned path.
- [Coverage gate ≥ 80% with a new crate] → the runpod crate mirrors the verda test suite (httpmock; pure selector tests; proptest for selector ranking per `testing.mdc`), which is what keeps the verda crate above the bar today.

## Migration Plan

1. Land config additively: `runpod:` disabled by default; existing deployments are unaffected except Verda's default `selection_strategy` flips to `best_value` (set `cheapest` explicitly to keep the old ranking). New `min_lifetime_seconds` defaults 0.
2. FleetState columns are additive; old state files load unchanged. Rollback = set `runpod.enabled: false`; managed pods can be drained via `POST /router/v1/runpod/destroy` or reclaimed by `destroy_on_shutdown`/orphan logic.
3. Metrics rename ships together with the Grafana dashboard update (single change, per repo rule).
4. Docs/rules/tests that encode "no RunPod" flip in the same change (see proposal Impact) so `task check` stays green at every commit.

## Open Questions

- Default value for the RunPod `image` tunable (which public CUDA-capable base image to recommend) — deferrable: any image with bash + curl works with the D7 bootstrap; the default can be tuned during apply without affecting specs or tasks.
