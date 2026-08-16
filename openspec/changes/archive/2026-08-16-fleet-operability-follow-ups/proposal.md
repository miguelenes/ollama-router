## Why

Five user-visible gaps remain after the honest-fleet work. `ollama stop` (a
generate with `keep_alive: 0`) unloads the model on only one ranked holder and
resets the idle timer, so spots stay alive and `ollama ps` still shows the
model loaded elsewhere. Operators cannot cordon a node for maintenance (the
`draining` flag and Prometheus gauge exist, but no admin route sets them).
Ranking ignores the CPU busy signal the node-agent already reports, so two
equally loaded CPU/overflow boxes tie even when one is pinned at 100%. A
wedged durable pull cannot be cancelled — admins can only watch it time out.
And where native Ollama queues briefly under load (`OLLAMA_MAX_QUEUE`), the
router 503s the instant every holder is saturated, so bursty `ollama run`
fails where a native daemon would have absorbed the burst.

## What Changes

- `POST /api/generate` / `POST /api/chat` with `keep_alive <= 0` and an empty
  or absent `prompt` / `messages` becomes a fleet **unload**: fan out to every
  healthy holder that has the model loaded (including operator-cordoned
  nodes; skip inventory/Verda teardown draining), reply with one
  CLI-compatible done/unload object. Not idle activity (matches pull/delete).
  Keeps the honest-fleet contract — this is a fleet operation, not a
  single-node passthrough.
- New admin routes `POST /router/v1/nodes/{id}/drain` and `/undrain`
  (fail-closed bearer). Operator drain is a **new cordon bit**, not the
  existing Verda/inventory `draining` flag (reusing that flag would drop
  fleet.yaml hosts on reload). Cordon excludes a node from ranking and
  placement targets but keeps health probes; it never destroys anything and
  never touches `fleet.yaml`.
- `load_key` gains a known-CPU-util term after known free VRAM (unknown is a
  middle band, not `0` — same discipline as GPU util and free VRAM; a node
  with omitted VRAM/GPU stays unknown, not a measured CPU). This takes **CPU
  util in the sort key** and **operator drain** out of the A-feature backlog.
- New admin route `POST /router/v1/jobs/{id}/cancel`: terminal-marks a running
  pull/delete job's incomplete targets; NDJSON watchers see a terminal error
  line; router-owned reasons only.
- Opt-in bounded saturation wait (`saturation_wait_seconds`, default `0` =
  off): when every eligible holder is saturated, wait for an inflight slot up
  to the knob before returning the existing 503 `all_nodes_saturated` +
  Retry-After. Streaming starts only after admission; nothing is buffered.

All other honest-fleet behavior is unchanged: list = union, infer = holders,
pull/delete = placement jobs, miss = 503, create/copy/push/blobs = 501.

**Non-goals (out-of-scope A-features):** native Hub-pull through one node,
default `auto_pull_on_miss`, 404-on-miss, agent-down unhealthy, public
tunnels, Thunder/RunPod, cross-request queue fairness or a persistent queue
(the wait is per-request and bounded), disk-pressure model eviction.

## Capabilities

### New Capabilities

- `api-stop`: `ollama stop` / `keep_alive <= 0` generate or chat is a fleet
  unload across loaded holders, operationally quiet for idle.
- `admin-node-drain`: operator drain/undrain of a node via the admin API
  (cordon bit, not inventory-remove draining); excludes from ranking and
  placement without destroying.
- `admin-job-cancel`: operator cancel of a running durable pull/delete job.

### Modified Capabilities

- `inference-routing`: the rank requirement gains a known-CPU-util tie-break
  after known free VRAM; a new requirement adds the opt-in bounded saturation
  wait before the existing 503.
- `size-load-routing`: new requirement — unknown CPU utilization is unknown,
  not `0` busy or `0` idle.

## Impact

- Crates: `ollama-router-core` (`routing/rank.rs`, `fleet/registry.rs` drain
  semantics for permanent nodes, `jobs/` cancel), `ollama-router` (`proxy/`
  unload + saturation wait, `http/admin.rs` drain/cancel routes, metrics).
- Config: `saturation_wait_seconds` policy knob (tunables-only YAML).
- Tests: rank unit tests, proxy tests (unload fan-out, wait/503), admin tests
  (drain, cancel), jobs tests (cancel terminal semantics).
- Docs: load-share, durable-model-operations, product, and idle-scale-down
  wiki; `routing-wlc` + `ollama-compat-proxy` + `idle-scale-down` skills
  (`.cursor` and `.opencode`); `openspec/config.yaml` context/rules (CPU util
  and operator drain leave the A-feature non-goal list).
- Sensitivity: unload detection reads `keep_alive`/`prompt` from the already
  parsed generate body — never log bodies; cancel and drain responses carry
  router-owned reasons only, never upstream detail text.
