# Context: Proxy & HTTP Surface (ollama-router)

> The Axum HTTP layer and the catch-all Ollama/OpenAI-compatible reverse proxy that rank, forward, and stream inference requests across the fleet.

**Type:** Service
**Created:** 2026-08-17
**Last Updated:** 2026-08-17
**Crate:** `ollama-router` (binary crate)
**Related Features:** none

## Overview

This is the public face of the router process: one listen URL (`:11434`) serving
Ollama-native (`/api/*`), OpenAI-compatible (`/v1/*`), admin (`/router/v1/*`),
health, metrics, and an embedded console UI. All non-special-cased traffic falls
through to a ranked reverse proxy that picks a node from the fleet registry,
reserves capacity, forwards with retry **only before the first upstream byte**,
and streams NDJSON/SSE back without buffering. The layer owns no state — fleet,
routing, jobs, and capacity logic live in `ollama-router-core`. The same crate
also hosts the background machinery: `tunnel.rs` (router-side zrok
private-access frontends for enrolled cloud nodes), `warm.rs` (warm-keeper that
preloads tier models without resetting idle), `health.rs` (per-node probe
supervisor), and `bootstrap.rs` (opt-in desired-tier ensure), plus the clap CLI
surface in `cli.rs`.

## Key Files

| File | Purpose |
|---|---|
| `crates/ollama-router/src/http/mod.rs` | `AppState`, `make_app()`, health/ready/metrics routes, `/router/ui` (rust-embed), tower-http layers |
| `crates/ollama-router/src/http/admin.rs` | All `/router/v1/*` handlers, bearer auth (`require_admin`), enroll, verda/runpod ops |
| `crates/ollama-router/src/http/metrics.rs` | Prometheus `Metrics` registry (~43 families), gauge refresh, `encode_text` |
| `crates/ollama-router/src/proxy/mod.rs` | `handle()` entry, `proxy_ranked()`, `forward_once()`, `ProxyStream`, fleet pull/delete/unload, error envelopes |
| `crates/ollama-router/src/proxy/telemetry.rs` | `IncrementalCollector` / `UpstreamTiming` (NDJSON + SSE frame parsing, 1 MiB cap) |
| `crates/ollama-router/src/tunnel.rs` | Router-side `zrok access private` frontends bound to loopback; enroll restore |
| `crates/ollama-router/src/warm.rs` | Background warm-keeper: occupy inflight without resetting idle |
| `crates/ollama-router/src/health.rs` | Per-node probe supervisor (tags/ps/capacity); `recheck_all`, fleet reload |
| `crates/ollama-router/src/bootstrap.rs` | Opt-in desired-tier bootstrap ensure after bind/reload |
| `crates/ollama-router/src/cli.rs` | clap CLI: `serve`, `ensure`, `delete`, `nodes`, `reload` |
| `crates/ollama-router/tests/proxy.rs` | httpmock integration tests for the proxy surface (plus `health.rs`, `healthz.rs`, `admin.rs`, `cli.rs`, `metrics_node.rs`) |

## Architecture

Request flow (catch-all path):

1. **Router** (`make_app`) registers explicit routes; everything else hits the
   `fallback(proxy_route)` → `proxy::handle`.
2. **`handle()`** special-cases aggregated GETs — `/api/tags`, `/api/ps`,
   `/api/version`, `/v1/models`, `/v1/models/{id}` — then rejects unknown
   `/v1/*` paths (404 OpenAI envelope) and non-fleet mutates
   (`/api/push|copy|create`, `/api/blobs` → 501).
3. **Body cap** (`read_body_capped`): `Content-Length` pre-check + `Limited`
   stream against `policy.max_request_body_bytes`; `model` extracted from JSON
   (with `name` fallback for `/api/show`).
4. **Fleet ops**: `/api/pull` → `fleet_pull` (job orchestrator, NDJSON stream);
   `/api/delete` → `fleet_delete`; generate/chat with `keep_alive <= 0` + empty
   prompt/messages → `fleet_unload` fan-out to every healthy holder.
5. **Rank + wait** (`proxy_ranked`): classify (`classify_with_size_hint`),
   `rank_nodes` from core; bounded waits on `Saturated` (slot notification or
   `saturation_wait_seconds`), `overload_wait_ms`, `admission_wait_ms`;
   `auto_pull_on_miss` (default off) enqueues a pull job and either waits
   (`auto_pull_wait_seconds`, 250 ms poll) or returns 503 + Retry-After + job id.
6. **Ranked retry loop**: `forward_once` per attempt; re-rank excluding tried
   nodes between attempts; `max_attempts = min(retry_max_attempts, ranked.len())`.
7. **`forward_once`**: semaphore permit (`upstream.max_connections`) →
   inflight inc + VRAM/RAM reservation (POST inference only) →
   `/api/embeddings` rewritten to `/api/embed` → send. Retryable statuses
   (`policy.retry_on_status`) and connect/timeout/protocol errors retry;
   streaming errors after first byte never retry.
8. **Stream**: `ProxyStream` pipes `bytes_stream()` chunks unchanged;
   `IncrementalCollector` parses timing frames; `InflightGuard` (RAII) releases
   inflight/reservations on drop.

```
client → make_app → explicit routes (tags/ps/version/models/health/admin/...)
                 ↘ fallback → proxy::handle → cap body → fleet ops? → rank →
                    wait → forward_once (reserve) → stream NDJSON/SSE back
```

Background (not in the request path): `health::run` probe supervisor (200 ms
supervisor tick, jittered per-node cadence), `TunnelFrontends` (zrok
`access private` children, one per enrolled share), the warm-keeper
(`warm::run`), and opt-in desired-tier bootstrap (`bootstrap::run`) — all
spawned at startup.

## Key Types & Modules

### `AppState` — `crates/ollama-router/src/http/mod.rs`

- **Purpose:** Cloned Axum state; production wiring via `from_config_with_shutdown`.
- **Key fields:** `config`, `registry`, `client` (reqwest), `orchestrator`
  (PullOrchestrator), `demand` (DemandScale), `pool` (Semaphore),
  `tie_break` (AtomicU64), `admin_token`, `fleet_state`, `verda`/`runpod`
  managers, `metrics`, `tunnels`.
- **Dependencies:** core config/fleet/jobs, verda, runpod crates.

### `ProxyCall` / `ForwardError` / `InflightGuard` / `ProxyStream` — `crates/ollama-router/src/proxy/mod.rs`

- **`ProxyCall`:** normalized request bundle for one ranked forward.
- **`ForwardError`:** `Overload` (retryable status) / `Retryable` (pre-byte
  transport) / `Fatal` (non-retryable → immediate 502).
- **`InflightGuard`:** RAII; drop releases inflight count + VRAM/RAM reservations.
- **`ProxyStream`:** `Stream<Item = Result<Bytes, io::Error>>`; marks
  success/failure on EOF/error; logs upstream timing; never buffers full body.

### `IncrementalCollector` / `UpstreamTiming` — `crates/ollama-router/src/proxy/telemetry.rs`

- **Purpose:** Best-effort telemetry from last complete NDJSON/SSE frame
  (durations, token counts incl. OpenAI `usage`); SSE mode keyed on
  `text/event-stream` Content-Type.
- **Invariant:** never mutates forwarded chunks; unterminated frame capped at
  `MAX_INCOMPLETE_FRAME_BYTES` (1 MiB).

### `Metrics` — `crates/ollama-router/src/http/metrics.rs`

- **Purpose:** Prometheus registry (~43 families, names are compile-time
  constants); `refresh_gauges`, `observe_request`, `route_reason`,
  `observe_discovery`, `observe_probe`, `encode_text` (`text/plain;
  version=0.0.4`), `stats_json` (admin `/router/v1/stats`).
- **Scrape model:** `/metrics` resets + repopulates every gauge family from a
  Registry + FleetState snapshot on each scrape (`refresh_gauges`). Counters
  and histograms accumulate.
- **Cardinality rules:** no model-name labels anywhere; hot series are
  `{node}`-labeled only; scrape the **router** (never node-agent `:11436`).
- **Measurement guards:** `*_known` companion gauges distinguish "measured 0"
  from "unknown" (e.g. `vram_free_gb=0` can be a full GPU).

| Group | Families (labels) |
|---|---|
| Requests & routing | `ollama_router_requests_total{class,code,node}` · `ollama_router_request_duration_seconds{class}` (wall time to response headers) · `ollama_router_route_reason_total{reason}` · `ollama_router_inflight{node}` · `ollama_router_auto_pull_wait_total{outcome}` · `ollama_router_probe_duration_seconds` |
| Node state | `node_healthy`, `node_draining`, `node_max_inflight`, `node_fail_streak`, `node_pressure` (0 unknown/1 ok/2 elevated/3 critical), `node_vram_gb`, `node_vram_free_gb`+`_known`, `node_vram_used_gb`+`_known`, `node_loaded_vram_gb`+`_known`, `node_reserved_vram_gb`, `node_ram_total_gb`, `node_ram_available_gb`+`_ratio`+`_known`, `node_gpu_utilization_pct`+`_known`, `node_cpu_usage_pct`+`_known`, `node_loaded_models`, `node_models`, `node_disk_available_gb`, `node_ollama_up` — all `{node}` |
| Node identity | `node_info{node,origin,role}` (origin=permanent\|verda\|runpod) · `backend_info{node,backend}` (cpu\|cuda\|rocm\|metal\|unknown) |
| Per-GPU | `node_gpu_vram_gb{node,gpu}`, `node_gpu_vram_free_gb{node,gpu}`+`_known`, `node_gpu_temperature_c{node,gpu}` |
| Cloud & jobs | `cloud_instances{provider}`, `cloud_price_per_hour{provider}`, `cloud_events_total{provider,event}`, `job_operations_total{kind,status}` |
| Discovery & tunnel | `aggregated_models`, `discovery_total{endpoint}` (tags\|openai_models\|ps\|version), `tunnel_up{node}` (1 = FleetState enroll has `tunnel_backend=zrok` + safe overlay URL) |

### `TunnelFrontends` — `crates/ollama-router/src/tunnel.rs`

- **Purpose:** Shared map share-id → loopback port; one `zrok access private`
  child per enrolled share (`--headless --bindAddress 127.0.0.1:<port>`),
  killed on drop. `restore_fleet` re-binds FleetState zrok shares after a
  router restart and persists the resulting loopback URLs back into FleetState.
- **Key methods:** `ensure(share_id)` (bind or reuse), `restore_fleet()`,
  `from_config()` (production), `zrok(bin)` (tests), `loopback()` (test
  stand-in answering `GET /api/tags` so enroll URLs are probeable).
- **Notes:** `zrok enable` runs at most once from the env var named by
  `tunnel.enable_token_env` (rejects multi-line tokens); never logs share or
  enable tokens.

### Warm-keeper — `crates/ollama-router/src/warm.rs`

- **Purpose:** Periodic background loop (`run`, gated on
  `policy.model_warm_enabled`) that finds cold-but-present tier models on
  healthy nodes and issues a tiny `POST /api/generate`
  (`num_predict: 1`, `stream: false`) to load them into VRAM.
- **Gates (per node):** pressure not `Critical`, inflight ratio ≤
  `model_warm_max_inflight_ratio`, `vram_free_gb` ≥ `model_warm_min_free_vram_gb`
  when known, per-node `model_warm_cooldown_seconds` cooldown, tier selection
  by known VRAM.
- **`OccupancyGuard`:** RAII; `occupancy_inc`/`occupancy_dec` occupy inflight
  during a warm request **without** resetting client-idle tracking.

### Health probe supervisor — `crates/ollama-router/src/health.rs`

- **Purpose:** One tokio task per live node; cycle = `GET /api/tags` (updates
  model catalog) → soft `GET /api/ps` → capacity probe every
  `capacity_probe_every_n_probes` cycles (agent `:11436` via core
  `CapacityClient`). Jittered intervals, registry-managed backoff, concurrency
  capped by `health.max_concurrent_probes`. Never counts as client inflight/idle.
- **Key fns:** `run()` (supervisor loop), `recheck_all()` (admin recheck),
  `reload_permanent_inventory()` (SIGHUP / `POST /router/v1/reload`: re-reads
  fleet.yaml in `spawn_blocking`, hydrates URLs from FleetState, applies
  inventory without interrupting inflight streams).
- **Hard rejection:** `public_url_blocked` URLs are marked unreachable, never
  healthy.

### Desired-tier bootstrap — `crates/ollama-router/src/bootstrap.rs`

- **Purpose:** Opt-in (`bootstrap_desired_models`) one-shot ensure of
  `desired_model_tiers` after `bootstrap_probe_wait_seconds`; does not block
  listen. Builds per-model placement targets via `bootstrap_targets` and starts
  a single orchestrator job (`start_ensure_targets`).
- **Gates:** known-VRAM tier match (`min_vram_gb`; unknown VRAM only matches
  `min_vram_gb == 0.0`), optional `bootstrap_require_capacity` (placement
  eligibility) and `bootstrap_require_ram_headroom`.

### CLI — `crates/ollama-router/src/cli.rs`

- **Purpose:** clap CLI; no Thunder or RunPod subcommands.
- **Subcommands:** `serve` (`--config`, `--host`/`--port` via
  `OLLAMA_ROUTER_HOST`/`OLLAMA_ROUTER_PORT`) · `ensure` / `delete` (repeatable
  `--model`, `--all-nodes`, `--nodes`, `--wait`) · `nodes` (inventory:
  `origin\tid\turl\ttunnel_backend\tenroll_age` — never share tokens) ·
  `reload` (POST `/router/v1/reload`, needs `OLLAMA_ROUTER_ADMIN_TOKEN`).

### Admin handlers (deep) — `crates/ollama-router/src/http/admin.rs`

- **Auth (`require_admin`):** fail-closed — unset `OLLAMA_ROUTER_ADMIN_TOKEN`
  → 403 "admin API disabled"; `Authorization: Bearer` compared constant-time
  (SHA-256 digests); mismatch → 401.
- **Model ops (`ensure_models` / `delete_models`):** `ModelOpRequest{models,
  nodes, wait}` + `?wait=`; `nodes: null` → `TargetSpec::Placement`, else
  explicit node list. 202 + `job_id` when async; 200 with final job when
  `wait` completes; 202 + `wait_timeout_seconds` on
  `ensure_wait_max_seconds` timeout. Orchestrator errors mapped via
  `map_orch_err` (422/404/409/503/502).
- **Jobs:** `list_jobs`, `get_job` (invalid id → 404), `cancel_job`.
- **Nodes:** `list_nodes` (public node views), `put_node` (debug adopt —
  never writes fleet.yaml), `drain_node` / `undrain_node`, `readiness` +
  `recheck` (immediate probe per non-draining node via `recheck_all`).
- **Enroll (`enroll_node`):** `EnrollRequest{id|proposed_id, origin,
  ollama_share_id, agent_share_id, agent_version, hostname}`. Flow: resolve id
  → reject public share tokens (vs `tunnel.public_share_suffixes`) → ownership
  mapping for `verda`/`runpod` origins (match FleetState row by id/hostname;
  must be `managed_by`-owned; fleet.yaml id → 409 `origin_mismatch`; unknown →
  404) → `tunnels.ensure()` for both shares (zrok frontend; failure → 502
  `zrok_access_failed`) → set node + capacity URLs → `persist_enroll_async`
  (**FleetState only** — never fleet.yaml, never SSH) → node public view +
  loopback URLs.
- **Verda / RunPod:** `{provider}_status/ensure/destroy` — 503 when disabled
  (`verda.enabled=false` / `runpod.enabled=false`); `ensure(true)` is
  adopt-first; `destroy` → 200, or 207 multi-status when any destroy failed;
  error text truncated to 200 chars.

## Storage

| Store | Purpose | Location |
|---|---|---|
| FleetState | durable node/tunnel ownership (read via core `FleetState`) | `config.state_path` (default `/var/lib/ollama-router/fleet-state.json`) |
| Job SQLite | pull/delete operation metadata (core `PullOrchestrator`) | `/var/lib/ollama-router/model-operations.sqlite3` |

(Neither is written by this layer directly — see `ollama-router-core` contexts.)

## HTTP Surface

### Ollama-compatible (public, unauthenticated)

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/tags` | aggregated model tags across healthy nodes |
| `GET` | `/api/ps` | aggregated running models |
| `GET` | `/api/version` | router version |
| `GET` | `/v1/models` | OpenAI-style model list (aggregated) |
| `GET` | `/v1/models/{id}` | single model by id (percent-decoded) |
| `POST` | `/api/generate`, `/api/chat`, `/api/embeddings` | inference (embedded rewrite → `/api/embed`) |
| `POST` | `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | OpenAI inference passthrough |
| `POST` | `/api/show`, `/api/pull` | model show / fleet pull (NDJSON job stream) |
| `POST` | `/api/push`, `/api/copy`, `/api/create` | explicitly registered but rejected — `unsupported_fleet_mutate` → 501 (`not_a_fleet_operation`) |
| `DELETE` | `/api/delete` | fleet delete (NDJSON job stream) |
| `GET` | `/healthz`, `/readyz`, `/metrics` | health / readiness / Prometheus |
| `GET` | `/router/ui`, `/router/ui/*` | embedded console UI (rust-embed) |
| `*` | fallback | everything else → `proxy::handle` |

### Admin `/router/v1/*` (bearer `OLLAMA_ROUTER_ADMIN_TOKEN`, fail-closed)

| Method | Path | Purpose |
|---|---|---|
| `GET`/`PUT` | `/nodes` | list nodes / debug adopt node |
| `POST` | `/nodes/{id}/drain`, `/nodes/{id}/undrain` | drain/undrain |
| `GET` | `/readiness` | readiness diagnostics |
| `POST` | `/readiness/recheck` | re-run probes |
| `GET`/`POST`/`POST` | `/models`, `/models/ensure`, `/models/delete` | model ops |
| `GET` | `/jobs`, `/jobs/{id}` | job list/detail |
| `POST` | `/jobs/{id}/cancel` | cancel job |
| `GET` | `/stats` | admin stats |
| `POST` | `/reload` | re-read config |
| `POST` | `/nodes/enroll` | zrok-share enroll (FleetState only) |
| `GET`/`POST`/`POST` | `/verda/status`, `/verda/ensure`, `/verda/destroy` | Verda spot ops |
| `GET`/`POST`/`POST` | `/runpod/status`, `/runpod/ensure`, `/runpod/destroy` | RunPod ops |

Full handler logic lives in `crates/ollama-router/src/http/admin.rs`
— see "Admin handlers (deep)" in Key Types below.

## Dependencies

### Internal
- `ollama-router-core` — `config` (RouterConfig/PolicyConfig/TimeoutsConfig),
  `fleet` (Registry, NodeSnapshot, FleetState, InflightAdmit), `routing`
  (rank_nodes, RequestClass, RoutingError, size hints), `jobs`
  (PullOrchestrator, Job, OrchestratorError), `http_util` (reqwest error mapping)
- `ollama-router-verda` — `VerdaManager`
- `ollama-router-runpod` — `RunpodManager`

### External
- axum 0.8 + tower-http (`TraceLayer`, request-id, sensitive headers)
- reqwest (rustls, `http1_only`, keepalive pool), tokio, futures-util,
  http-body-util (`Limited`), bytes, serde_json, rust-embed, prometheus

## Patterns & Conventions

- **Stream, don't buffer** — responses are `Body::from_stream`; retry only
  pre-first-byte (`forward_once` returns before `bytes_stream()`).
- **Cap before buffering** — request bodies go through `read_body_capped`.
- **Error envelope by path** — OpenAI `{"error": {message, type, code}}` on
  `/v1/*`, Ollama `{"error": "..."}` on `/api/*`; `Retry-After` on 503s.
- **Reason codes** — every router rejection carries a stable
  `RoutingError::as_reason_code()` string (e.g. `no_healthy`, `saturated`,
  `public_url_blocked`); used for metrics and logs.
- **Sensitivity** — never log bodies/prompts; upstream error strings truncated
  to 200 chars (`truncate`); `Authorization` marked sensitive via
  `SetSensitiveRequestHeadersLayer`; hop-by-hop headers stripped both ways.
- **Debug headers** — `x-ollama-router-upstream`, `x-ollama-router-class`,
  `x-ollama-router-aggregated` only when `config.debug_headers`.
- **RAII accounting** — `InflightGuard` owns inflight/reservation release.
- **Soft probes** — health/ps/capacity probes and warm requests never count
  toward client idle; probe failures flip node health, never a client error.
- **Admin fail-closed** — unset admin token disables `/router/v1/*` (403);
  bearer compared constant-time; upstream error text capped at 200 chars.
- **Rewrite compat** — `/api/embeddings` → `/api/embed` (Ollama ≤ 0.32);
  `/v1/embeddings` is never rewritten.
- **No model-name metric labels**; `/metrics` and `/healthz` stay unauthenticated.

## Edge Cases & Gotchas

- **Saturation wait race** — `proxy_ranked` arms the registry slot
  notification *before* re-ranking, so a slot released between check and wait
  is never lost.
- **Auto-pull poll** — `wait_for_pull` polls every 250 ms; if the model
  appears on an eligible node mid-wait, the *original request* is forwarded
  (re-ranked) instead of returning 503.
- **Sticky affinity tie** — a sticky owner promotes only when its full load
  key equals the best candidate; `tie_break` (AtomicU64) feeds a
  deterministic tie-breaker into `rank_nodes`.
- **Unload fan-out** — targets every healthy holder *including cordoned*
  (inventory-draining excluded); any per-node failure → 502 `unload_failed`;
  no holders → success (idempotent).
- **`keep_alive` parsing** — accepts Go-style durations (`1h30m`); unparseable
  values fall through to normal inference (never an unload).
- **Model id decoding** — `/v1/models/{id}` percent-decodes the id; invalid
  UTF-8 falls back to the raw input.
- **`/api/show` name fallback** — model extracted from `name` when `model` is
  absent (Ollama CLI uses `name`).
- **Oversized frame recovery** — an unterminated NDJSON/SSE frame past 1 MiB
  is discarded until the next newline, then parsing recovers; forwarded
  chunks are never mutated.
- **Retry boundary** — `retry_on_status` and transport errors retry only
  before the first upstream byte; a mid-stream error marks the node failed
  but never retries.
- **Guard releases** — `InflightGuard` releases inflight + VRAM/RAM
  reservations on drop, so retryable failures clean up before the next
  attempt; failures still call `mark_request_failure` /
  `mark_request_overload`.
- **Body cap** — negative `Content-Length` → 400 `invalid_content_length`;
  over-cap → 413 (OpenAI envelope on `/v1/*`, Ollama `{"error"}` on
  `/api/*`).
- **Tunnel restore** — `restore_fleet` re-binds enrolled shares on restart;
  persisted loopback URLs use ephemeral ports and are re-written to FleetState.
- **Probe jitter** — `jittered_interval` clamps the ratio to 0..1 with a
  0.05 s floor; unhealthy nodes back off via the registry's probe backoff.

## Known Issues / Technical Debt

- None known.

## Notes

- The upstream reqwest client is rustls-only (`build_upstream_client`); no
  native-tls/openssl anywhere in the workspace.
- Idle-tracking (`inflight_inc`) and VRAM/RAM reservations happen **only** on
  POST inference paths (`is_client_forward`); health/ps/admin do not count.
- `keep_alive` parsing accepts Go-style duration strings (`1h30m`, `-1m`) — see
  `parse_go_duration_seconds`; unparseable values fall through to normal inference.
- Integration tests use httpmock and live under `crates/ollama-router/tests/`;
  unit tests sit next to the modules (unload intent, collector caps).
