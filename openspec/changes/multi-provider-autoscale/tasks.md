## 1. Core cloud policy generalization

- [x] 1.1 Rename `VerdaInstanceId` → `CloudInstanceId` in `ollama-router-core` (mechanical, no alias) and fix all call sites
- [x] 1.2 Add `NodeOrigin::Runpod` with `as_str() == "runpod"`; keep `Permanent`/`Verda` semantics unchanged
- [x] 1.3 Parameterize `cloud/mod.rs` policy fns by provider origin (drop hardcoded `origin == NodeOrigin::Verda` in `idle_eligible` / `excess_scale_down_order`); unit tests: fleet.yaml hosts still never returned (cloud-autoscale "fleet.yaml hosts are never destroyed")
- [x] 1.4 Add `min_lifetime: Duration` to `IdlePolicy`; instance younger than it is never an idle candidate; unit tests for "Minimum lifetime is honored" and "Fine-grained billing tears down promptly" scenarios
- [x] 1.5 Generalize `FleetEvents::verda_event(event)` → `cloud_event(provider, event)`; update `NoopFleetEvents` and all emitters

## 2. Config: runpod block and guard-test flips

- [x] 2.1 Add `runpod:` tunables to `router.defaults.yaml` (disabled by default): `enabled`, `api_key_env: RUNPOD_API_KEY`, `base_url_v1`, `base_url_v2`, `cloud_type`, `interruptible: true`, `on_demand_fallback: false`, `image`, `container_disk_gb`, `min_vram_gb`/`max_vram_gb`, `allowed_gpu_types`/`denied_gpu_types`, `allowed_data_centers`, `max_price_per_hour`, `auto_scale` + `auto_scale_min_instances`/`auto_scale_max_instances`, `idle_*` + `orphan_reclaim_*` + `destroy_on_shutdown` mirroring `verda:`, `min_lifetime_seconds: 0`, poll/create/destroy timeouts, agent/zrok/enroll env-name knobs
- [x] 2.2 Add `min_lifetime_seconds: 0` to the `verda:` block and thread both providers' values into `IdlePolicy`
- [x] 2.3 Flip Verda default `selection_strategy` from `cheapest` to `best_value` in `router.defaults.yaml` and `VerdaConfig`; keep `cheapest` as an explicit opt-in. Do not treat `demand_scale_price_per_hour` as a cap (metrics/log fallback only)
- [x] 2.4 Config structs + validation in `config/models.rs`/`load.rs`: `runpod.enabled` without the API key env set is a hard startup error naming the env var (spec: "Enabled without credentials fails closed"); no secrets accepted in YAML; a set `verda.max_spot_price_per_hour` or `runpod.max_price_per_hour` MUST be `> 0`
- [x] 2.5 Flip guard tests in `config/load.rs`: `runpod:` overlay now parses; `thunder:` stays an unknown-field error; defaults snapshot test expects a `runpod:` block and Verda `selection_strategy: best_value`
- [x] 2.6 Update `config.overlay.example.yaml` with a commented runpod example (no secrets) and a note that Verda `max_spot_price_per_hour` / RunPod `max_price_per_hour` are the hourly caps
- [x] 2.7 Verda selector unit tests: default best-value picks lower $/VRAM over cheaper sticker (cloud-autoscale "Better value wins over cheaper sticker"); over-cap offer rejected; unknown-VRAM loses a value comparison; explicit `cheapest` still ranks by hourly price

## 3. crates/ollama-router-runpod

- [x] 3.1 Scaffold crate (thiserror `RunpodError`, reqwest rustls-tls, workspace lints); consult Context7 reqwest docs before writing the client
- [x] 3.2 `client.rs`: bearer-key client — v1 `POST /pods`, `GET /pods`, `GET /pods/{id}`, `DELETE /pods/{id}`; v2 `GET /v2/catalog/gpus?include=AVAILABILITY`; serde ignore-unknown DTOs; 429 backoff honoring `Retry-After`/`RateLimit`; httpmock tests incl. unknown-fields leniency (spec: "Unknown fields are ignored")
- [x] 3.3 `selector.rs` (pure): filter available + VRAM band + allow/deny + data centers + `max_price_per_hour` cap, rank by price per VRAM GiB, emit ordered `gpuTypeIds`; unit tests for "Cheapest per-GiB eligible GPU wins", "Over-cap GPU is skipped even if it would win on value", and "Nothing eligible means no create"; proptest that the cap/band filter dominates ranking (per `testing.mdc`)
- [x] 3.4 `startup.rs`: render agent-bootstrap script (agent package install, zrok private enable, agent serve, enroll) as `dockerStartCmd`; env carries only names from `*_env` knobs; `ports: []`, `volumeInGb: 0`; test asserts no token material appears in the rendered pod-create body or logs
- [x] 3.5 `manager.rs`: pod-name managed marker (router id embedded), `is_owned` by name scheme + FleetState row; coalesced `create_additional` (interruptible-first, on-demand fallback only when enabled — spec "No silent on-demand fallback"); post-create `costPerHr` cap verify-and-terminate (design D5); `run_reconcile_loop` with floor create, idle/excess teardown via core policy, interruption cleanup (terminate non-RUNNING managed pods; replace only below floor — both spec scenarios), orphan reclaim, `destroy_on_shutdown`; teardown always terminates, never stops (spec: "Teardown terminates permanently")
- [x] 3.6 httpmock manager tests mirroring the verda suite: coalesced create, ceiling refusal, foreign pod untouched, interrupted-below-floor replaced, interrupted-above-floor waits, failed destroy retains FleetState row, no-secret logging on create failure

## 4. FleetState and enroll

- [x] 4.1 Add optional `runpod_pod_id` / `runpod_gpu_type` / `runpod_data_center` / `runpod_cost_per_hour` columns and `managed_by="runpod"` helpers (`list_runpod_nodes`, snapshot filter); old state files load unchanged (additive-only test)
- [x] 4.2 Enroll `origin=runpod` in `http/admin.rs`: only updates an existing `managed_by=runpod` row, mirroring `verda_not_owned` conflict (spec: "Enroll cannot invent a RunPod node"); never writes `fleet.yaml`
- [x] 4.3 Registry/health: RunPod nodes enter with `NodeOrigin::Runpod`; loopback zrok URL becomes healthy, public pod IP / RunPod proxy hostname stays `public_url_blocked` (tests for both spec scenarios)

## 5. Multi-provider demand and wiring

- [x] 5.1 `CloudProviderHandle` trait + `MultiProviderDemand` in core `cloud/` (design D1): best cached value score (lowest $/known VRAM GiB) below ceiling wins, unknown VRAM sorts last, equal scores break by lower hourly price, stockout falls back, provider without cached offer skipped; unit tests for "Better-value provider wins" and "Stockout falls back"
- [x] 5.2 Implement the handle on `VerdaManager` and `RunpodManager` (each refreshes a cached `CachedOffer` — hourly price plus optional known VRAM — on its reconcile tick)
- [x] 5.3 Wire `main.rs`/`bootstrap.rs`: spawn each enabled manager's reconcile loop, set `state.demand = MultiProviderDemand`; single-provider setups degenerate to current behavior (verda-only integration test stays green)
- [x] 5.4 Proxy integration test: capacity miss still returns 503 + `Retry-After` immediately and triggers exactly one coalesced create on the chosen provider; health//api/ps/admin/warm-keeper traffic never scales (cloud-autoscale scenarios)

## 6. Admin and CLI

- [x] 6.1 Add `/router/v1/runpod/{status,ensure,destroy}` mirroring verda routes (fail-closed bearer); rework `tests/admin.rs::no_thunder_or_runpod_admin_paths` to keep only the Thunder half
- [x] 6.2 CLI `nodes`: map `managed_by="runpod"` → `origin=runpod` with id/url/tunnel_backend/enroll_age, no token material (spec: "Nodes listing shows RunPod origin"); update `ollama-router-verda/src/tests.rs` no-runpod string assertion accordingly

## 7. Metrics and dashboard

- [x] 7.1 Replace verda-named series with provider-labeled ones (design D9): `ollama_router_cloud_instances{provider}`, `ollama_router_cloud_events_total{provider,event}`, `ollama_router_cloud_price_per_hour{provider}`; `node_info` origin gains `runpod`; no model-name labels; metrics test asserts per-provider attribution (spec: "Scale decisions are visible per provider")
- [x] 7.2 Update fleet home `deploy/observability/grafana/dashboards/ollama-router.json`: retitle the "cpu / gpu / verda" row; instance, price, idle, and demand panels use the new series split by `provider`; keep it the home dashboard (`GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH` unchanged)
- [x] 7.3 Retitle `ollama-router-verda.json` to Cloud; keep UID `ollama-router-verda`; add `$provider` (`verda`/`runpod`); rewrite panels and `origin="verda"` filters; update cross-link titles that say "Verda" to "Cloud" on this and sibling dashboards
- [x] 7.4 Update `ollama-router-nodes.json` Verda instance/price/events section to the new series split by provider (and accept `origin=runpod`)
- [x] 7.5 Rewrite alert `VerdaEnsureFailed` in `deploy/observability/rules/ollama-router.yml` to `CloudEnsureFailed` on `ollama_router_cloud_events_total{event="ensure_failed"}`; update the nodes-dashboard `ALERTS{alertname=...}` matcher (spec: "Compose dashboards and the ensure-failed alert follow provider labels")

## 8. Docs, rules, skills

- [x] 8.1 Amend `AGENTS.md`, `.cursor/rules/fleet-invariants.mdc`, `.cursor/rules/testing.mdc`, and `openspec/config.yaml` (context + rules): RunPod is now a supported provider; Thunder stays forbidden; tunnel/loopback-only and never-destroy-fleet.yaml invariants unchanged; `RUNPOD_API_KEY` joins the never-log list
- [x] 8.2 Update `.opencode/wiki/concepts/` (product, idle-scale-down, node-tunnel) for the two-provider model and RunPod container bootstrap
- [x] 8.3 Update `verda-spot-fleet` skill scope note, add a RunPod pod-fleet skill under `.cursor/skills/` + `.opencode/skills/`, and retitle the Verda row in the `grafana-mcp` skill dashboard table to Cloud (same UID)
- [x] 8.4 Update `.opencode/wiki/index.md` and any docs referencing "Verda-only" wording found via repo grep for `runpod`/`thunder`

## 9. Gate

- [x] 9.1 `cargo fmt --all`, clear IDE diagnostics on edited files, then sequential `task check` (fmt --check, clippy -D warnings, test --locked, deny) until green
- [x] 9.2 `task coverage` ≥ 80% lines (new runpod crate included; no crate exclusions, no threshold change)
