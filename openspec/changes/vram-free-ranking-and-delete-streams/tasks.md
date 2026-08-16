## 1. Ranking: known free VRAM

- [ ] 1.1 Extend `load_key` with a free-VRAM term after inflight/cap, RAM pressure, and GPU util (2 GiB tight band; unknown is middle, not `0`). Do not change metrics `vram_free_gb.unwrap_or(0)` callers gated by `vram_free_known`. Do not hard-filter on unknown free
- [ ] 1.2 Rank unit tests: equal inflight/RAM/GPU-util → known 8 GiB free beats known 0.5 GiB; 2/8 @ 20 GiB free loses to 1/8 @ 0.5 GiB; GPU util 10% @ 1 GiB free beats 80% @ 20 GiB; known 8 GiB free beats unknown; unknown beats known 0 GiB free. Class-preference fixtures may leave free VRAM unknown (equal middle band). Keep existing WLC tests (utilization beats preference, saturated never selected)

## 2. Proxy: delete NDJSON

- [ ] 2.1 `DELETE /api/delete` streams the fleet delete job via Axum `Body::from_stream` (`application/x-ndjson`), reusing the pull job watcher (`total`/`completed` from target counts). Final `{"status":"success"}` on success-like jobs including already-absent / `NoTargetNodes`. Partial failure → error object, no success, no upstream `detail`. Client drop does not cancel the job. Missing `model` → 400. Not idle. Do not log bodies
- [ ] 2.2 Proxy tests: successful delete is NDJSON with a success line (including already-absent); two holders are both targets; delete does not set `last_client_request_at`; missing name is 400 without a stream

## 3. Docs and gate

- [ ] 3.1 Product wiki (`ollama-router-load-share.md`, `ollama-router-durable-model-operations.md`) + `ollama-compat-proxy` + `routing-wlc` skills (`.cursor` and `.opencode`) + `openspec/config.yaml`: known free VRAM is a soft tie-break after GPU util (unknown ≠ 0 full); `DELETE /api/delete` streams like pull. Drop `vram_free` from A-feature non-goals; keep CPU util in the sort key as an A-feature
- [ ] 3.2 Run `task check`
