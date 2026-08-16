## 1. Ranking: known CPU util

- [ ] 1.1 Add `cpu_util_key` to `load_key` after free VRAM (busy band 80%; `None` is the middle band, not idle `0`); tuple becomes `(inflight, pressure, gpu_util, vram_free, cpu_util, preference, warm+ram)`. Do not touch metrics gauges
- [ ] 1.2 Rank unit tests: known 20% beats known 95% on equal prior fields; known 10% beats unknown; unknown beats known 95%; inflight 1/8 @ 95% CPU beats 2/8 @ 10%; known 8 GiB free @ 90% CPU beats 0.5 GiB free @ 10% CPU. Class-preference fixtures stay equal on the CPU band (unknown). Keep existing WLC/proptest suites green

## 2. Operator cordon (drain API)

- [ ] 2.1 Registry `cordoned` bit + snapshot field, set/cleared only by admin; ranking, placement targets, bootstrap, and warm-keeper exclude `draining || cordoned`; health/capacity probes continue; `should_remove_permanent` and Verda teardown still read only `draining`; `apply_permanent_config` must not clear `cordoned` (unlike inventory `draining`)
- [ ] 2.2 Admin routes `POST /router/v1/nodes/{id}/drain` and `/undrain` (fail-closed bearer; unset → 403; unknown id → 404; idempotent); nodes listing shows the state; `ollama_router_node_draining` reports `draining || cordoned`
- [ ] 2.3 Tests: drained holder is never selected (sole holder → existing 503); drained fleet.yaml host survives reload reconcile, stays drained, and keeps probes; undrain restores ranking; 403 with unset token; gauge flips

## 3. Fleet unload (`ollama stop`)

- [ ] 3.1 Detect unload-intent (`keep_alive <= 0` number or duration string, empty/absent `prompt`/`messages`) before ranking; fan out native unload to every healthy node with the model in the ps union, including operator-cordoned holders, excluding inventory/Verda `draining`; router-owned `done_reason: "unload"` success object (zero targets = success); any target failure → 502 router-owned error, no upstream text; missing `model` → 400; no `inflight_inc`, no `last_client_request_at`; never log bodies
- [ ] 3.2 Proxy tests: two loaded holders both receive unload; unloaded model returns success; non-empty prompt with `keep_alive: 0` stays ranked inference; unload still hits a cordoned loaded holder; unload does not set `last_client_request_at`

## 4. Job cancel

- [ ] 4.1 `TargetStatus::Cancelled` (failure-like summary; additive SQLite string; unknown status parses as failed) + `cancel_job`: unknown → 404-mapped error, terminal → conflict, else mark incomplete targets cancelled, stamp `finished_at`, persist, notify; dispatch skips non-incomplete targets; `set_target` refuses to overwrite terminal target status
- [ ] 4.2 Admin route `POST /router/v1/jobs/{id}/cancel` (fail-closed bearer) + tests: cancel running pull → terminal non-success and NDJSON watcher gets a terminal error line, no success; cancel terminal job → 409 unchanged; unknown id → 404; unset token → 403; store round-trips the new status

## 5. Bounded saturation wait

- [ ] 5.1 `saturation_wait_seconds` policy knob (default 0) + registry `Notify` fired from `inflight_dec`; proxy loops re-rank/wait under a deadline on `RoutingError::Saturated`, forwards on a freed slot, otherwise existing 503 `all_nodes_saturated` + Retry-After; wait ends on client disconnect; never forward to a saturated node
- [ ] 5.2 Proxy tests: freed slot admits the waiting request and streams; deadline expiry is the existing 503 with Retry-After; default 0 keeps the immediate 503

## 6. Docs and gate

- [ ] 6.1 Update wiki (`ollama-router-load-share.md`, `ollama-router-durable-model-operations.md`, `ollama-router-product.md`, `ollama-router-idle-scale-down.md`) + `routing-wlc`, `ollama-compat-proxy`, `idle-scale-down` skills (`.cursor` and `.opencode`) + `openspec/config.yaml` context/rules: CPU util joins the sort key and operator drain exists (both leave the A-feature non-goal list); stop is a fleet unload; jobs are cancellable; saturation wait is opt-in
- [ ] 6.2 Run `task check`
