# Design

## Context

Three workstreams, all inside the existing architecture: the Axum 0.8 router binary (`crates/ollama-router/src/http/` routes + `proxy/` dispatch + `metrics.rs`), `crates/ollama-router-core` (registry `NodeOrigin`, routing errors), the Verda/RunPod managers (`FleetEvents` trait), and the node-agent crate with its per-OS packaging scripts. Key constraints shaping the approach: the honest-fleet contract stays (miss = 503 `model_missing`, mutate = 501, infer = holders-only); hot Prometheus metrics never label by model name; the admin bearer stays fail-closed; production Prometheus scrapes only the router (never node-agent `:11436`); `healthz` is the documented liveness probe and must stay pure liveness (Docker HEALTHCHECK curls it).

## Goals / Non-Goals

**Goals:**

- Make every proxy path deterministic (405/404/501 instead of accidental upstream passthrough) without changing the behavior of supported operations.
- Give adopt nodes a true origin, uncoupled from Verda counting/reclaim, with RunPod parity in readiness.
- Make packaging artifacts verify against the binary and make setup failures loud.
- Close the RED/log visibility gaps on rejection and job paths, and make dashboards honest about what production scrapes.

**Non-Goals:**

- No ranking changes: WLC sort key, unknown-VRAM ≠ 0, GPU-first/CPU-overflow, and class preference are untouched.
- No durable-job admin endpoint (audit finding b6 is deferred; SQLite stays recovery-only).
- No removal of the Windows `setup` firewall rules or the legacy schtasks cleanup (documented, load-bearing migration behavior) — only documentation of the env knobs.
- No per-node duration histogram, no production scrape job for `:11436`, no new cloud provider, no Thunder.
- `/readyz`'s embedding-model gate stays exactly as-is; only the saturation condition is added.

## Decisions

### Feature gaps

**D1 — Explicit routes + a dispatch-level status table.** Register explicit Axum routes for the core endpoints currently served through the fallback (`/api/generate`, `/api/chat`, `/api/embed`, `/api/embeddings`, `/api/ps`, `/api/version`, `/api/blobs/*`, `/api/stop`) pointing at the same proxy handler, and maintain a path × allowed-methods table consulted by `proxy::handle()` so a known path with the wrong method returns **405** and an unknown `/api/*` or `/v1/*` path returns **404** — each with the envelope matching the prefix (Ollama error object vs OpenAI error envelope), no upstream contact, no inflight/idle movement, bodies read only up to the existing cap. Status precedence: the 501 mutate/blobs check wins first (unchanged for any method on `/api/create`, `/api/copy`, `/api/push`, `/api/blobs/*`), then known-path wrong-method 405, then unknown-path 404. On the OpenAI side the `openai_unknown_path` branch splits into known-path-wrong-method (405) versus truly unknown (404); the existing unknown-path 404 test stays green, and wrong-method on a known OpenAI path changes 404 → 405 (no test pinned the old value). *Alternative rejected:* keep the catch-all and add method checks inside `handle()` only — it hides the route surface and makes 405s easy to regress; explicit `route()` entries make drift visible to the existing admin-route-count test. *Alternative rejected:* 404 for every mismatch — 405 on a known path is the honest HTTP signal and matches what clients expect.

**D2 — `/api/stop` reuses the existing fleet-unload path.** Route `POST /api/stop` into the same fan-out machinery as unload-intent generate/chat (`proxy/mod.rs` fleet unload: every healthy + cordoned loaded holder, `done_reason: "unload"`, idempotent, 502 on any target failure, no idle/inflight movement). Only the model extraction differs (body field `model`; missing → 400). *Alternative rejected:* 501 for `/api/stop` — unload is not a mutate; fan-out is the honest fleet interpretation and `api-stop` already owns this contract.

**D3 — `NodeOrigin::Adopt` variant in core.** Add `Adopt` to `NodeOrigin` (`crates/ollama-router-core/src/fleet/registry.rs`) and route admin `PUT /router/v1/nodes` (new id) and `enroll` `origin: adopt` through a new `upsert_adopt` instead of `upsert_verda`. `upsert_adopt` records `origin=Adopt` with `managed_by: None` — FleetState `managed_by` is already `Option<String>` (`fleet/state.rs`), and Verda/RunPod counting, reclaim, and idle views filter on `Some("verda")` / `Some("runpod")`, so unset ownership excludes adopt rows automatically; no new `managed_by` value is introduced. Serde is additive — existing FleetState files stay valid; only new adopt rows use the new origin value. `nodes` CLI, readiness counts, and `node_info` metrics report `origin="adopt"`. *Alternative rejected:* keep Verda origin plus a flag — one enum meaning two things is exactly the current bug.

**D4 — Readiness aggregates both managers.** `/router/v1/readiness` builds counts from the registry (permanent/adopt/verda/runpod) and merges recovery from both enabled managers' snapshots (Verda + RunPod) instead of consulting only Verda. *Alternative rejected:* extend counts only — recovery parity is the observable fix and the spec scenario.

**D5 — OpenAPI parity test.** Fix the "capacity" wording in `site/openapi/openapi.yaml` and add a test asserting the documented `operationId` set equals the admin route table (mirrors the existing 21-operations check), so drift fails CI.

### Agent cleanup

**D6 — Delete dead code; no re-wiring.** Remove `LiveGpuProbe` and the `GpuProbe` trait entirely (both are stubs; tests exercise parsers directly), remove `windows_silent_args`, drop the unused `pub use` re-exports in `collect/mod.rs`, and consolidate the five timestamp helpers into one shared `now_rfc3339()` used by `setup/{linux,macos,windows}.rs`, `collect`, and `pressure`. *Alternative rejected:* wire `LiveGpuProbe` into `collect_live` — no caller needs the seam; a synthetic-inventory path is the opposite of the "never invent inventory" contract.

**D7 — Generated plist check mirrors the Linux unit check.** Add a hidden `setup --print-plist` output (agent + tunnel, byte-stable template strings like `agent_unit_text()`) and make `pack-pkg.sh` diff binary output against the checked-in plists, exactly like `pack-deb.sh`/`pack-tarball.sh` do for units; the tautological `cmp` is replaced. *Alternative rejected:* dropping the check — Linux units are already check-verified; macOS deserves the same and the spec requires it.

**D8 — Tunnel daemon ships disabled.** The macOS pkg ships `com.ollama.node-agent.tunnel.plist` with launchd `Disabled=true` and the postinstall boot-out ensures it does not auto-start; `setup` enables/bootstraps it only after share reservation succeeds. The agent daemon plist is unchanged. *Alternative rejected:* not shipping the tunnel plist at all — it is still the setup-converged artifact and removing it would diverge Linux/macOS convergence.

**D9 — Loud setup failures.** The swallowed `std::fs::write` of the Ollama env file on macOS becomes an error return (reuse the `write_bytes_idempotent` pattern from Linux), so `setup` exits non-zero naming the path. Single-version-probe: capture `ollama_version()` once per converge in `setup/macos.rs` and `setup/windows.rs` and reuse the value.

**D10 — Agent metrics renamed in place.** `ollama_up` → `ollama_node_agent_ollama_up`, `ollama_models` → `ollama_node_agent_models`, `ollama_gpu_vram_gb` → `ollama_node_agent_gpu_vram_gb`, `ram_available_gb` → `ollama_node_agent_ram_available_gb`, `gpu_utilization_pct` → `ollama_node_agent_gpu_utilization_pct`. Production is unaffected (nothing scrapes `:11436`); mock compose dashboards referencing the old names are updated in the same change. Env knobs `OLLAMA_NODE_AGENT_DISCOVERED_V4` and `OLLAMA_NODE_AGENT_WINDOWS_ZIP` are documented (kept as test seams), nfpm description aligned, and `dtolnay/rust-toolchain@stable` pinned to a version tag in `release-agent.yml`.

### Observability

**D11 — One local-response observer helper.** A small `observe_local(class, status, reason, model: Option<&str>)` helper in the proxy/http layer that increments `requests_total{class,code}`, calls `route_reason(reason)`, and emits one reason-coded `tracing` event with allowlisted fields (path, class, status/reason, model when present). It is called at: body-cap 400/413, the new wrong-method 405 and unknown-Ollama-404 local responses, `openai_unknown_path` 404, 501 mutates, `openai_model_by_id` 404, and the pull/delete/unload failure branches. The class label is the same `classify(path, model, policy)` result the proxy already computes — `Pull` for `/api/pull` (per `request-class`), `Embed` for embed endpoints, `Generic` for `/api/show`, model-derived for inference paths, and `Generic` for paths with no model and no path class (unknown paths, 501 mutates, `/api/delete`). Pull failure branches record the **wire** reason code the branch carries: `insufficient_capacity` for zero placement-eligible targets (matches `api-pull`); the `NotConfigured` 503 — which today has no reason code — gains `no_nodes_configured` so every rejection carries a reason (R1). *Alternative rejected:* instrumenting each branch ad hoc — five duplicated call-site patterns is how the current gaps happened.

**D12 — `reason` label on `cloud_events_total`.** The `FleetEvents` emission signature gains an optional reason parameter; Verda/RunPod managers pass the triggering route reason on demand events and a failure class on `ensure_failed`/destroy failures; the counter becomes `ollama_router_cloud_events_total{provider,event,reason}` with empty-string reason for no-reason events. PromQL selectors that don't constrain `reason` (the existing `ensure_failed` alert, provider-split panels) keep matching; dashboards gain reason-aware panels. *Alternative rejected:* a separate `*_reason_total` counter — two counters for one event stream, and the spec literally says "labeled by provider and reason".

**D13 — Gauges for in-flight work.** `ollama_router_jobs_running{kind}` driven by a `job_started`/`job_terminal` pair on the existing `JobObserver` (orchestrator notifies on enqueue/terminal states); `ollama_router_upstream_pool_available` sampled from the semaphore's `available_permits()` in `refresh_gauges()`. Jobs dashboard's "no in-flight gauge" note is removed once the gauge exists.

**D14 — Dashboards reference only emitted series.** The nodes dashboard is mounted by both prod and mock compose (shared `compose.stack.yaml`), so it goes router-only: replace `ollama_up`/`ollama_models` (agent-only) with `ollama_router_node_ollama_up`/`ollama_router_node_models` and remove the router-vs-agent comparison panels from the shared dashboard rather than renaming them. Agent-series references live only in the documented mock-only stack dashboard (`compose-scrapes.json`), updated to the renamed `ollama_node_agent_*` names. Add a `ollama_router_tunnel_up{node}` panel. All additive edits — the home dashboard (`ollama-router`) is not replaced.

**D15 — Saturation-aware `/readyz`.** `readyz` reuses the routing saturation predicate (inflight ≥ effective max) on the healthy non-drained snapshot: 503 when none of them can accept a request, 200 otherwise. `healthz` untouched. *Alternative rejected:* a new readiness endpoint — `/readyz` already means "can serve"; duplicating the signal splits operators' probes. Note: with unknown capacity data the predicate errs toward "can serve" (mirrors the routing hard-filter's handling of unknown caps).

**D16 — Model on routing logs.** `route_rejected`/`route` debug events add the already-allowlisted model field where the request carries one.

## Risks / Trade-offs

- **[405/404 tightening could surface as regressions for clients that accidentally relied on passthrough]** → those calls were never contract; envelope shapes are preserved and every new status gets a proxy test. Release notes call out the change (including OpenAI wrong-method 404 → 405).
- **[`/readyz` 503 during load peaks may flap health-aware orchestration]** → `healthz` remains pure liveness and is the documented probe; the wiki documents the new semantics before merge.
- **[Adding `reason` raises cardinality]** → reason values are bounded (route-reason enum + failure classes + empty string), documented, and the label is additive to existing selectors.
- **[`NodeOrigin::Adopt` serialization]** → additive serde variant; a fixture test loads a pre-change FleetState to prove compatibility.
- **[Agent metric renames break mock compose]** → mock dashboards updated in the same change; production scrape is unaffected by design.
- **[Plists must be byte-stable to diff cleanly]** → template strings (not serialized plists) generated by the binary, matching the unit-text pattern.
- **[macOS/Windows converge code is only compile-verified in `release-agent.yml`]** → changes are mechanical (error propagation, single probe, plist text) and the packaging tests run in CI on release.

## Migration Plan

- No FleetState data migration: existing rows keep their origin; only new adopt rows use `origin: adopt`. A pre-change binary does not recognize the new origin string, so rolling back requires re-enrolling affected adopt nodes (a debug-only surface, acceptable for rollback).
- Agent metric renames are mock-compose-only; ship with the dashboard edits.
- `/readyz` semantics change is documented in the wiki in the same change; single-router-replica deployment means no split-brain window.

## Open Questions

None — decisions that would change specs or the task breakdown were resolved above (405 vs 404 on wrong method, `/api/stop` fan-out vs 501, metric label vs new counter, `/readyz` extension vs new endpoint).
