## 1. Feature gaps — deterministic proxy statuses (inference-routing, api-stop)

- [ ] 1.1 Register explicit Axum routes in `make_app()` for `/api/generate`, `/api/chat`, `/api/embed`, `/api/embeddings`, `/api/ps`, `/api/version`, `/api/blobs/*`, and `POST /api/stop`, all pointing at the existing proxy handler; the catch-all fallback remains only for paths with no explicit route.
- [ ] 1.2 Add a known-path × allowed-method status table consulted by `proxy::handle()`, with precedence: the 501 mutate/blobs check first (unchanged for any method on `/api/create`, `/api/copy`, `/api/push`, `/api/blobs/*`), then wrong method on a known path → 405 (Ollama error object on `/api/*`, OpenAI error envelope on `/v1/*`), then unknown `/api/*` or `/v1/*` path → 404 with the matching envelope. Split the `openai_unknown_path` branch into known-path-wrong-method (405) versus truly unknown (404); keep the existing unknown-path 404 test green. No upstream contact, no inflight/idle movement (scenarios: `POST /api/tags` 405, `GET /v1/chat/completions` 405, unknown `/api/*` 404).
- [ ] 1.3 Return 501 with an OpenAI error envelope for `DELETE /v1/models/{id}` and `POST /v1/fine_tuning/*` (scenarios: OpenAI model delete 501, fine-tuning 501).
- [ ] 1.4 Implement `POST /api/stop`: parse `model` (missing → 400); otherwise run the existing fleet-unload fan-out (every healthy + cordoned loaded holder, one `done: true` + `done_reason: "unload"` response, idempotent for unloaded models, 502 with router-owned reason on any target failure, no `last_client_request_at`/inflight movement) — reuse the unload machinery, leave unload-intent generate/chat untouched (scenarios: two-holder fan-out, unloaded success, missing-model 400, idle timer unchanged).
- [ ] 1.5 Add `tests/proxy.rs` coverage: 405/404/501 shapes for both envelopes with httpmock assertions that no upstream request was made; `/api/stop` fan-out, 400, and idle-timer invariants.

## 2. Feature gaps — adopt origin and readiness parity (admin-nodes)

- [ ] 2.1 Add `NodeOrigin::Adopt` to `crates/ollama-router-core/src/fleet/registry.rs` (additive serde) and route `PUT /router/v1/nodes` (new id) and `enroll` with `origin: adopt` through a new `upsert_adopt` instead of `upsert_verda`; `upsert_adopt` records `origin=Adopt` with `managed_by: None` (FleetState `managed_by` is already optional) (scenarios: admin-created node is not Verda, enroll adopt does not collide with reclaim).
- [ ] 2.2 Verify and regression-test that Verda registered-scale counting and orphan reclaim exclude adopt rows (they key off FleetState `managed_by=verda`); adopt nodes stay out of Verda idle teardown.
- [ ] 2.3 Extend `/router/v1/readiness`: per-origin counts for `permanent`, `adopt`, `verda`, `runpod`; recovery field merges snapshots from both enabled cloud managers (scenarios: RunPod reconciling visible, adopt counted separately).
- [ ] 2.4 Report `origin="adopt"` in the `nodes` CLI output and the `node_info` metric; add a FleetState fixture test proving a pre-change state file loads after the enum addition.

## 3. Feature gaps — docs parity (router-console, public-docs-site)

- [ ] 3.1 Fix the phantom "capacity" wording in `site/openapi/openapi.yaml` to list the real `/router/v1/*` surface, and add a test asserting the documented `operationId` set equals the registered admin route table (scenario: documented operations match the shipped routes).
- [ ] 3.2 Verify the console integration test covers unauthenticated asset serving, index fallback for client-side routes, and fail-closed 403 data access; add missing assertions (scenarios: console opens without a token, client-side route fallback, console data fail-closed).

## 4. Agent cleanup — crate code (node-agent)

- [ ] 4.1 Remove dead code: `LiveGpuProbe` stub and `GpuProbe` trait, `windows_silent_args` test-only duplicate, and unused `pub use` re-exports in `collect/mod.rs`.
- [ ] 4.2 Consolidate the five timestamp helpers (`now_rfc3339` ×3, `rfc3339_now`, `epoch_now`) into one shared helper used by `setup/{linux,macos,windows}.rs`, `collect`, and `pressure`.
- [ ] 4.3 Probe the installed Ollama version at most once per converge in `setup/macos.rs` and `setup/windows.rs` (scenario: single version probe per converge).
- [ ] 4.4 Propagate the macOS Ollama env-file write failure so `setup` exits non-zero with the path named (scenario: env-file write failure fails setup).
- [ ] 4.5 Rename agent metrics to `ollama_node_agent_ollama_up`, `ollama_node_agent_models`, `ollama_node_agent_gpu_vram_gb`, `ollama_node_agent_ram_available_gb`, `ollama_node_agent_gpu_utilization_pct`; update agent metric tests and the mock compose dashboards that reference the old names (scenario: agent metric families are prefixed).
- [ ] 4.6 Document `OLLAMA_NODE_AGENT_DISCOVERED_V4` and `OLLAMA_NODE_AGENT_WINDOWS_ZIP` in the shipped config comments and the node-agent docs; align the nfpm description with the crate description (scenario: no undocumented knobs).

## 5. Agent cleanup — packaging and CI (node-agent)

- [ ] 5.1 Add a hidden `setup --print-plist` output (agent + tunnel, byte-stable template strings mirroring `agent_unit_text()`).
- [ ] 5.2 Replace the tautological `cmp` in `packaging/macos/pack-pkg.sh` with a diff of the binary-generated plist text against the checked-in plists, failing the build on drift — the same pattern as `pack-deb.sh`/`pack-tarball.sh` (scenarios: drifted plist fails the build, matching artifacts package cleanly).
- [ ] 5.3 Ship the macOS tunnel LaunchDaemon disabled (`Disabled=true` + postinstall boot-out) and have `setup` enable/bootstrap it only after share reservation; the agent daemon plist stays unchanged (scenarios: fresh pkg install does not crash-loop, setup activates the tunnel).
- [ ] 5.4 Add packaging-consistency tests for the plist output and a failure-path test for `pack-pkg.sh` drift detection.
- [ ] 5.5 Pin `dtolnay/rust-toolchain@stable` to a version tag in `.github/workflows/release-agent.yml` (never a commit SHA).

## 6. Observability — metrics and logs (router-observability, cloud-autoscale)

- [ ] 6.1 Add a local-response observer helper that increments `requests_total{class,code}`, records a `route_reason`, and emits one reason-coded tracing event (path, class, status/reason, model when present — no bodies); call it from body-cap 400/413, the new wrong-method 405 and unknown-Ollama-404 responses, `openai_unknown_path` 404, 501 mutates, `openai_model_by_id` 404, and pull/delete/unload failure branches. Class label = the `classify()` result the proxy computes for the path (`Pull` for `/api/pull` per `request-class`, `Generic` fallback for no-model/no-path-class paths). Pull branches record the wire reason code: `insufficient_capacity` for zero placement-eligible targets; give the un-reasoned `NotConfigured` 503 the `no_nodes_configured` reason code so every rejection carries one (scenarios: rejections are counted, pull branches are counted, unknown-path rejection reaches Loki).
- [ ] 6.2 Add a `reason` label to `ollama_router_cloud_events_total` (empty string when none); Verda/RunPod `FleetEvents` emissions pass the triggering route reason on demand and a failure class on `ensure_failed`/destroy failures; verify the existing `ensure_failed` alert and provider-split panels still match (scenario: demand records the trigger reason).
- [ ] 6.3 Add `ollama_router_jobs_running{kind}` driven by `job_started`/`job_terminal` on the existing `JobObserver`, and `ollama_router_upstream_pool_available` sampled from the connection-pool semaphore in `refresh_gauges()` (scenario: a running pull is visible in Prometheus).
- [ ] 6.4 Extend `/readyz`: 503 when there are no healthy non-draining nodes OR every healthy node is saturated (reuse the routing saturation predicate; unknown capacity errs toward can-serve); `healthz` stays pure liveness; the embedding-model gate is unchanged (scenarios: saturated-only fleet is not ready, headroom means ready).
- [ ] 6.5 Add the allowlisted model name to `route_rejected`/`route` debug log events when the request carries one (scenario: model miss logs the model).
- [ ] 6.6 Add tests: `tests/metrics_node.rs` for the new gauges/labels and reason values; `tests/proxy.rs` for counted rejections on every instrumented branch; readyz saturation/headroom cases; log-field assertions on the rejection events.

## 7. Observability — dashboards (router-observability)

- [ ] 7.1 Nodes dashboard (shared by prod and mock compose): replace `ollama_up`/`ollama_models` references with `ollama_router_node_ollama_up`/`ollama_router_node_models` and remove the router-vs-agent comparison panels (they query agent-only series production never scrapes); add a `ollama_router_tunnel_up{node}` panel (scenarios: production nodes panels resolve to router series, tunnel-down node is visible).
- [ ] 7.2 Jobs dashboard: remove the "no in-flight gauge" note and add a running-jobs panel; cloud dashboards: reason-aware event panels. All edits additive — the home dashboard (`ollama-router`) is not replaced.
- [ ] 7.3 Update the documented mock-only stack dashboard (`compose-scrapes.json`) for the renamed `ollama_node_agent_*` series; the shared nodes dashboard keeps no agent-series references.

## 8. Docs, wiki, skills, and the finish gate

- [ ] 8.1 Update `.opencode/wiki/concepts/ollama-capacity-discovery.md` (documented env knobs, agent metric prefix) and `.opencode/wiki/concepts/ollama-router-product.md` (saturation-aware `/readyz` semantics).
- [ ] 8.2 Update the `capacity-wire` skill (agent metric names) and the `ollama-compat-proxy` skill (405/404/501 status contract, `POST /api/stop` fan-out); update the node-agent docs page (`site/src/content/docs/guides/node-agent.md`) for env knobs, metric prefix, and the disabled-by-default macOS tunnel daemon.
- [ ] 8.3 Run the sequential gate to green: `task check` (cargo fmt --check, clippy --workspace --all-targets -- -D warnings, cargo test --workspace --locked, cargo deny) and `task coverage` (line coverage ≥ 80%). Fix every failure; do not lower the floor or add allows.
