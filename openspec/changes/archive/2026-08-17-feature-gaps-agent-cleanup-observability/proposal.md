## Why

A systematic audit of the router proxy surface, the node-agent crate/packaging, and the router observability stack found real, user-visible gaps that tests don't catch: wrong-method requests on Ollama/OpenAI paths leak upstream by accident instead of getting a deterministic 404/405, adopt nodes are labeled `origin=Verda` and inflate Verda scale-up caps, readiness ignores RunPod, several proxy rejection paths emit no metrics or logs at all, and the nodes dashboard queries node-agent series that production never scrapes. The honest-fleet contract is kept intact — this change closes gaps around it, it does not reverse it.

## What Changes

Three workstreams from a one-time audit (findings are the source of truth for tasks):

**Feature gaps (proxy/admin surface)**
- Wrong-method and unknown-path requests on the Ollama (`/api/*`) surface stop leaking upstream and get deterministic 404/405 responses; OpenAI (`/v1/*`) wrong-method moves from 404 to 405 for consistency; core inference endpoints move from the catch-all fallback to explicit routes. Mutate/blobs paths keep their 501 for every method.
- Literal `POST /api/stop` performs the same fleet unload fan-out as unload-intent generate/chat (today it is forwarded to one ranked node as Generic).
- Known-but-unsupported OpenAI mutation paths (`DELETE /v1/models/{id}`, `POST /v1/fine_tuning/*`) return 501 like the native mutate endpoints, not an indistinguishable 404.
- Adopt nodes (admin `PUT /router/v1/nodes` with a new id, `enroll` with `origin: adopt`) get their own origin instead of `origin=Verda`; they no longer count toward Verda scale-up caps or orphan reclaim.
- `/router/v1/readiness` counts and recovery cover RunPod alongside Verda.
- The `/router/ui` console gets its first spec; the OpenAPI doc wording drift ("capacity") is fixed.

**Agent cleanup (node-agent crate + packaging)**
- Remove dead code (`LiveGpuProbe` stub, test-only `windows_silent_args`), consolidate five timestamp helpers, and make converge probe the Ollama version once instead of twice.
- macOS pkg no longer ships a tunnel LaunchDaemon that crash-loops on hosts that never ran `setup`; setup surfaces Ollama env-file write failures instead of swallowing them; `pack-pkg.sh` compares binary-generated plist text against the checked-in file (mirroring the Linux unit check) instead of a tautological `cmp`.
- Document the two undocumented env knobs; align the nfpm description; rename unprefixed agent metrics (`ollama_up`, `ollama_models`, `ram_available_gb`, `gpu_utilization_pct`, `ollama_gpu_vram_gb`) under the `ollama_node_agent_` prefix.

**Observability (metrics, dashboards, readiness)**
- Every request path — including body-cap 400/413, unknown-path 404, 501 mutates, pull/delete/unload branches, and OpenAI model-by-id 404 — increments `requests_total`, records a route reason, and emits a reason-coded log line.
- `cloud_events_total` gains a `reason` label (satisfies the autoscale spec's "reason where applicable"); an in-flight jobs gauge and an upstream-pool gauge are added.
- Dashboards query only series production actually scrapes (nodes dashboard switches to `ollama_router_node_ollama_up` / `ollama_router_node_models`); `ollama_router_tunnel_up` gets a panel.
- `/readyz` returns 503 when every healthy node is saturated (healthz stays pure liveness); the `route` debug log includes the model name.

## Capabilities

### New Capabilities

- `admin-nodes`: node registration origins (adopt distinct from Verda/RunPod, no Verda cap/reclaim coupling) and readiness counts/recovery covering both cloud providers.
- `router-console`: `/router/ui` console surface served without admin auth; data only via the authenticated admin API.
- `node-agent`: agent packaging and convergence hardening — no crash-looping tunnel daemon from a pkg install, packaging verifies generated artifacts against binary output, setup surfaces write failures, single version probe, documented env knobs, consistent `ollama_node_agent_` metric prefix.
- `router-observability`: RED metric completeness on every request path, reason-coded rejections and logs, in-flight jobs gauge, upstream pool gauge, dashboards referencing only emitted series, tunnel-up visibility, saturation-aware `/readyz`.

### Modified Capabilities

- `inference-routing`: deterministic 404/405 for wrong-method and unknown Ollama/OpenAI paths (no accidental upstream passthrough); known OpenAI mutation paths return 501.
- `api-stop`: literal `POST /api/stop` follows the fleet-unload contract (fan-out, `done_reason: "unload"`, idempotent, 502 on target failure).
- `cloud-autoscale`: cloud event metrics labeled by provider and reason.
- `public-docs-site`: OpenAPI doc wording fixed to match the shipped `/router/v1/*` surface (no phantom "capacity" operation).

## Impact

- **Crates**: `ollama-router` (http routes, proxy dispatch, metrics, readiness, admin), `ollama-router-core` (NodeOrigin + registry upsert, routing error surface), `ollama-router-verda` / `ollama-router-runpod` (manager status for readiness + reason-labeled events), `ollama-node-agent` (collect/setup/listen/metrics + packaging scripts + plist/unit text), `ollama-capacity-types` only if agent metric renames cross the DTO boundary (they do not — agent-local only).
- **Deploy/observability YAML**: Grafana dashboards (`ollama-router-nodes.json`, `ollama-router-jobs.json`, mock `compose-scrapes.json` for agent metric renames), no scrape-config changes (never add a `:11436` production job), alert rules untouched except verifying cloud-event alerts still match after the `reason` label lands.
- **Tests**: `tests/proxy.rs` (405/404/501 shapes, `/api/stop` fan-out), `tests/admin.rs` (adopt origin, readiness RunPod), `tests/metrics_node.rs` (new gauges, reason label), agent `tests/http.rs` + packaging-consistency tests, `service_identity` tests for the plist check.
- **Docs/skills/wiki**: `site/src/content/docs/guides/node-agent.md`, `site/openapi/openapi.yaml`, `.opencode/wiki/concepts/ollama-capacity-discovery.md` (env knobs), `capacity-wire` skill (agent metric names), `ollama-compat-proxy` skill (status-code contract).
- **CI**: pin `dtolnay/rust-toolchain@stable` to a version tag in `release-agent.yml`.
- **Sensitivity**: new 405/404 paths parse no bodies beyond the existing cap; new log lines and metrics stay within the allowlist (node id, model name, class, status, latency, reason codes). No bodies, prompts, tokens, or share secrets are logged; no metric gains a model label.
