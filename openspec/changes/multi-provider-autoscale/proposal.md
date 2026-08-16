# Multi-Provider Autoscale (Verda + RunPod)

## Why

When the Verda spot pool is out of stock or priced badly, clients keep receiving 503 `insufficient_capacity` / `all_nodes_saturated` even though the operator would happily pay for a GPU on another marketplace. Operators also cannot express a per-provider autoscale envelope — a warm floor to keep latency down and a hard ceiling to bound spend — beyond the single Verda knob pair. This change reverses the RunPod half of the "Verda spots only" invariant: RunPod's per-second-billed interruptible pods become a second, independently enabled cloud provider behind one provider-agnostic autoscale contract.

## What Changes

- **Invariant reversal (deliberate)**: RunPod becomes a supported cloud provider. Thunder stays forbidden. The `runpod:` tunables block becomes valid YAML (today `config/load.rs` tests assert it is rejected as an unknown field) — **BREAKING** only for the internal guard tests and docs that encode "no RunPod"; no client-facing API breaks.
- **Provider abstraction**: the core cloud reconcile loop (`ollama-router-core/src/cloud/`) drives N providers through a common provider trait instead of the concrete Verda manager. Demand scale-up stays coalesced-on-capacity-miss with **503 + Retry-After** to the client; idle scale-down stays driven solely by `last_client_request_at`; fleet.yaml hosts are never destroyed. The honest-fleet contract (list = union, infer = holders, pull = placement, miss = 503, mutates = 501) is fully kept.
- **Per-provider autoscale envelope**: `min_instances` / `max_instances` per provider (Verda already has `auto_scale_min_instances` / `auto_scale_max_instances`; RunPod gets the same). Scale-up happens only under real load (capacity miss / saturation), never speculatively above the floor.
- **New crate `crates/ollama-router-runpod`**: bearer-key REST client (v1 `rest.runpod.io/v1` for pod create with `interruptible: true`, list, terminate; v2 `api.runpod.io/v2/catalog/gpus` for GPU price + availability discovery), a best-price-per-VRAM selector within the configured VRAM band, and a manager with terminate-permanently semantics (RunPod `stop` still bills volume storage; we terminate).
- **Cost-optimal lifecycle**: interruptible (spot) first with configurable on-demand fallback, per-provider price cap, and billing-aware teardown — a per-provider minimum-lifetime / billing-granularity knob so instances are kept through the minimum billed period and torn down at the cost-optimal moment instead of thrashing (RunPod bills per second; Verda differs).
- **FleetState + enroll**: RunPod pods are recorded as `managed_by=runpod` rows; enroll `origin=runpod` only updates an existing managed row (mirror of the Verda rule); enroll never writes `fleet.yaml`.
- **Tunnel invariant unchanged**: RunPod pod Ollama URLs must be the self-hosted zrok private share (loopback). The pod's public IP / RunPod proxy is never used for Ollama traffic and stays `public_url_blocked`. Bootstrap is the pod's Docker image + env (RunPod runs containers, not startup scripts); the router never SSHes.
- **Interruption recovery**: an interrupted (EXITED) spot pod is terminated and replaced through the same coalesced demand path, not restarted in place.
- **Observability**: per-provider scale-decision counters/gauges (no model-name labels); compose Grafana fleet dashboard gains provider breakdowns.

### Non-Goals

- Thunder remains forbidden (env, routes, tests, docs).
- No public tunnels: RunPod's proxy/public IP never becomes a healthy Ollama URL.
- No RunPod Serverless endpoints, Instant Clusters, or savings-plan management — Pods only.
- No change to the honest-fleet surface: no native Hub-pull through one node, `auto_pull_on_miss` stays default false, miss stays 503 (not 404), agent-down stays soft-fail.
- No second router replica, no Redis; FleetState stays the single-writer file.
- No changes to ranking/placement semantics (unknown VRAM stays unknown, not 0; known CPU stays `vram_gb: 0, gpus: 0`). Cloud nodes enter ranking exactly as Verda nodes do today.

## Capabilities

### New Capabilities

- `cloud-autoscale`: provider-agnostic autoscale contract — per-provider enable, min/max envelope, load-only scale-up with coalescing and 503 + Retry-After, idle scale-down from `last_client_request_at`, billing-aware minimum lifetime, never destroying fleet.yaml hosts, orphan reclaim, and provider-tagged observability.
- `cloud-provider-runpod`: RunPod-specific behavior — catalog-driven best price-per-VRAM GPU selection inside the VRAM band, interruptible-first rental with optional on-demand fallback and price cap, pod bootstrap via Docker image + env with zrok-private-share-only URLs, `managed_by=runpod` FleetState ownership and enroll, terminate-permanently teardown, and spot-interruption replacement.

### Modified Capabilities

<!-- No existing main spec covers cloud provisioning; Verda behavior exists only as archived change deltas. Existing capabilities (inference-routing, size-load-routing, api-*) keep their requirements unchanged. -->

## Impact

- **Crates**: new `crates/ollama-router-runpod`; `crates/ollama-router-core` (`cloud/` reconcile generalization, `config/` new `runpod:` block + validation, `fleet/` state origins); `crates/ollama-router` (bootstrap wiring, admin nodes listing origin values, metrics registration, CLI `nodes` output).
- **Config**: `router.defaults.yaml` gains a disabled-by-default `runpod:` block; `config.overlay.example.yaml` updated. API key via `api_key_env: RUNPOD_API_KEY` — secrets stay in env, never YAML.
- **Tests that currently enforce no-RunPod and must be reworked**: `config/load.rs` (runpod overlay rejected, defaults contain no `runpod:`), `crates/ollama-router/tests/admin.rs::no_thunder_or_runpod_admin_paths` (keep the Thunder half), `crates/ollama-router-verda/src/tests.rs` no-runpod string assertion. Plus new unit/integration tests for the provider trait, RunPod selector, and reconcile envelope.
- **Docs/rules/skills to amend in the same change**: `AGENTS.md`, `.cursor/rules/fleet-invariants.mdc`, `.cursor/rules/testing.mdc`, `openspec/config.yaml` (context + rules mention "no RunPod"), `.opencode/wiki/concepts/` (product, idle-scale-down, node-tunnel), `verda-spot-fleet` skill (scope note) and a new RunPod fleet skill (`.cursor/skills/` + `.opencode/skills/`).
- **Dependencies**: reqwest `rustls-tls` only (already in workspace); RunPod DTOs use serde ignore-unknown-fields (same rule as Verda). No new TLS or GraphQL dependencies — REST only.
- **Sensitivity**: `RUNPOD_API_KEY` joins the never-log list alongside Verda tokens. Pod-create env carries the zrok enable token and enroll bearer — memory-only, never logged or persisted. Logging allowlist stays: node id, GPU type, data center, price, VRAM GiB, reason codes.
- **Observability**: new per-provider metric labels/series and a compose Grafana dashboard update (fleet overview stays the home dashboard).
