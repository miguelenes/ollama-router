## 1. Ranking: known GPU util

- [ ] 1.1 Extend `load_key` with a GPU-util term after inflight/cap and RAM pressure (50% busy band; unknown is middle, not `0`). Do not change metrics `gpu_util_pct.unwrap_or(0)` callers gated by `gpu_util_known`
- [ ] 1.2 Rank unit tests: equal inflight → known 10% beats known 80%; 2/8 @ 10% loses to 1/8 @ 90%; known 10% beats unknown; unknown beats known 90%. Class-preference fixtures may leave GPU util unknown (equal middle band). Keep existing WLC tests (utilization beats preference, saturated never selected)

## 2. Placement: known disk skip + bootstrap

- [ ] 2.1 Skip pull/ensure targets when `disk_available_gb` is known and below the size estimate (target node’s tag `size`, else aggregated catalog `size`; GiB = bytes / 1024³). Unknown disk and missing/`0` size do not skip. Add `TargetStatus::SkippedDisk`; do not persist `detail` strings
- [ ] 2.2 Placement/orchestrator tests: known 1 GiB free + 5 GiB `size` skips; unknown disk stays eligible; LARGE default placement still omits known CPU and unknown VRAM
- [ ] 2.3 When `bootstrap_desired_models` is true, background-ensure `desired_model_tiers` after bind and on reload (`bootstrap_probe_wait_seconds`, `bootstrap_require_capacity`, `bootstrap_require_ram_headroom`). Intersect generate-class eligibility with known `min_vram_gb`. Default false starts no jobs. Do not block listen
- [ ] 2.4 Tests: bootstrap false does not ensure; bootstrap LARGE skips CPU and unknown VRAM; `min_vram_gb` 24 excludes a known 8 GiB node
- [ ] 2.5 Warm-keeper: still on-disk only (no pull). Tier pick uses known VRAM (`min_vram_gb`); unknown VRAM only `min_vram_gb == 0`. Skip free VRAM only when `vram_free_gb` is known and below `model_warm_min_free_vram_gb` (unknown free MUST NOT encode as `0`). Do not change metrics gauges
- [ ] 2.6 Tests: warm-keeper does not pull; unknown free + unknown VRAM + `min_vram_gb` 0 on-disk model is not skipped as empty; unknown VRAM does not warm a `min_vram_gb` 24 on-disk model

## 3. Proxy: ps union, holder show, pull NDJSON

- [ ] 3.1 Store per-node `PsRecord`s from health `/api/ps` (ignore unknown JSON fields). Serve `GET /api/ps` as a fleet union (one row per healthy node × loaded model, `details.router_node`, digest ≥ 12 chars). Do not passthrough one node. Do not log bodies
- [ ] 3.2 Proxy tests: two loaded holders → two rows; unhealthy omitted; empty → `{"models":[]}` with no upstream client `GET /api/ps`; listing does not set `last_client_request_at`
- [ ] 3.3 Forward `POST /api/show` only to a healthy holder (`name` or `model`); miss 503 `model_missing`; still GENERIC (unknown-VRAM holder of a 70B name is shown, not `insufficient_capacity`); stream the upstream body (do not buffer); not idle; do not log bodies
- [ ] 3.4 Proxy tests: show hits the holder not the non-holder; miss 503; GENERIC class header; inflight/idle unchanged
- [ ] 3.5 `POST /api/pull` streams placement-job NDJSON via Axum `Body::from_stream` (`application/x-ndjson`), `total`/`completed` from target counts, final `{"status":"success"}` on success-like jobs. Zero targets → 503 `insufficient_capacity` without success. Partial failure → error object, no success, no upstream `detail`. Missing `model` → 400 without a stream. Client drop does not cancel the job. Not idle
- [ ] 3.6 Proxy tests: successful pull is NDJSON with a success line (including already-present); two eligible GPUs are both targets; LARGE against CPU+unknown is 503; missing `model` is 400 without a stream; pull does not set `last_client_request_at`
- [ ] 3.7 Serve `GET /api/version` as router-owned `{"version": env!("CARGO_PKG_VERSION")}` (same as `/healthz`). Do not `proxy_ranked`. Do not log the body. Not idle
- [ ] 3.8 Proxy tests: version body matches `/healthz` version; upstream `/api/version` is not contacted; `last_client_request_at` unchanged

## 4. Docs and gate

- [ ] 4.1 Product wiki (`ollama-router-product.md`, `ollama-router-load-share.md`) + `ollama-compat-proxy` + `routing-wlc` skills (`.cursor` and `.opencode`) + `openspec/config.yaml` context: `ps`/`show`/`pull`/`version` match the honest fleet; pull streams placement NDJSON (not a one-node Hub-pull); bootstrap is opt-in; GPU util, disk, and warm skips are known-only. Drop stale A-feature non-goals for fleet-union `/api/ps` and GPU-util-in-the-sort-key
- [ ] 4.2 Run `task check`
