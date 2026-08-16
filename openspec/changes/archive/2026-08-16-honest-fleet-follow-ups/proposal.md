## Why

`ollama list` is a fleet union, but `show` / `ps` / `pull` / `version` still look like a single daemon (wrong node, one node's loaded set, no pull progress, a random Ollama build). Desired model tiers are GitOps decoration: `bootstrap_desired_models` is never read, so new GPUs stay empty until someone `ensure`s. Ranking still cannot see live GPU util even though the agent already reports it — two equal-inflight cards look the same while one is busy. The warm-keeper still encodes omitted VRAM/free as `0`, so an unknown GPU never warms.

## What Changes

This change **keeps** the honest-fleet contract: list = union of holders; infer = holders only; pull = placement job (not a native Hub-pull through one node); miss = 503 `model_missing`; create/copy/push/blobs stay 501; `auto_pull_on_miss` stays default **false**.

- `GET /api/ps` becomes a **process-list union** of currently loaded models on healthy, non-draining nodes (one row per node that has the model resident, `details.router_node`), not a single-node passthrough and not a tags-style one-row-per-name collapse.
- `POST /api/show` is forwarded only to a **healthy holder** of the named model (holder filter, still GENERIC — not LARGE-gated). Miss is 503 `model_missing`, not a native 404 from the wrong node.
- `POST /api/pull` stays a placement job and **streams NDJSON progress** the official CLI can consume (status / totals). It does not become a single-node native pull.
- When `bootstrap_desired_models` is true, the router **ensures** `desired_model_tiers` onto healthy generate-class-eligible nodes (same LARGE/MEDIUM known-VRAM gates), intersecting each tier’s `min_vram_gb` with **known** VRAM only. Warm-keeper still only warms models already on disk, but tier membership and free-VRAM skips use **known** values (omitted MUST NOT encode as `0`). Default remains **false**.
- Ranking **applies** a **soft** known-GPU-util penalty after inflight utilization **and** RAM pressure. Unknown GPU util MUST NOT sort as `0`. Inflight still dominates. Unknown VRAM vs measured CPU (`vram_gb = 0`, `gpus = 0`) is unchanged.
- Placement/pull **skips a node only when disk free is known and below** the pull estimate (probe `size` when present). Unknown disk (agent down / omitted) MUST NOT encode as `0` and MUST NOT skip. Inference ranking is unchanged.
- `GET /api/version` is **router-owned** (`{"version": "<router version>"}`, same as `/healthz`), not a GENERIC passthrough to one node's Ollama.
- Docs/wiki + `ollama-compat-proxy` / `routing-wlc` skills + `openspec/config.yaml` context: `ps`/`show`/`pull`/`version` match the honest fleet; desired bootstrap is opt-in; GPU util, disk, and warm skips are known-only.

Not in this change: default `auto_pull_on_miss`, 404-on-miss, making agent-down nodes unhealthy, reversing 501 mutates, inference-path auth, a request queue instead of 503, operator drain API, public tunnels, Thunder/RunPod, `vram_free`/CPU util in the sort key, `DELETE /api/delete` NDJSON.

## Capabilities

### New Capabilities

- `api-ps`: Aggregated `GET /api/ps` is the process-list union of loaded models on healthy, non-draining nodes (one row per node; CLI `ps` against the router URL).
- `api-show`: `POST /api/show` is forwarded to a holder of the named model; not a GENERIC passthrough to an arbitrary ranked node.
- `api-pull`: `POST /api/pull` streams placement-job NDJSON progress while still targeting generate-class-eligible nodes.
- `api-version`: `GET /api/version` returns the router process version, not a ranked node's Ollama build.

### Modified Capabilities

- `model-placement`: Opt-in `bootstrap_desired_models` actually places the configured desired tiers (healthy, generate-class, known-VRAM gates, known `min_vram_gb`). Known-insufficient disk skips a pull target; unknown disk does not. Warm-keeper tier membership and free-VRAM skips use known values only.
- `inference-routing`: Known GPU utilization biases ranking after inflight and RAM pressure; it MUST NOT override a utilization gap.
- `size-load-routing`: Unknown GPU util is unknown, not zero (same honesty as omitted VRAM).

## Impact

- `crates/ollama-router/src/proxy/` — aggregate `ps`, holder-only `show`, NDJSON pull progress, router-owned `version`; do not log bodies.
- `crates/ollama-router/src/health.rs` + `crates/ollama-router-core/src/fleet/` — persist per-node ps probe records; client `GET /api/ps` is a dedicated union (health probes already exist).
- `crates/ollama-router/src/warm.rs` — known VRAM for tier pick; unknown free VRAM does not skip as `0`.
- `crates/ollama-router-core/src/jobs/` + `src/routing/place.rs` — bootstrap ensure; known-disk skip; pull stream from job status (metadata only in SQLite). Snapshot already has `disk_available_gb`.
- `crates/ollama-router-core/src/routing/rank.rs` — known `gpu_util` term after inflight and RAM pressure; `gpu_util_known` false must not encode as 0.
- `crates/ollama-router-core/src/config/` — wire `bootstrap_desired_models` (already parsed) plus `bootstrap_probe_wait_seconds` / `bootstrap_require_capacity` / `bootstrap_require_ram_headroom`.
- Tests: proxy `ps`/`show`/`pull` stream/`version`; bootstrap skip CPU/unknown VRAM for LARGE; known-low disk skip vs unknown-disk still eligible; GPU-util tie vs inflight-dominates; unknown-util not preferred as idle; warm unknown free does not skip; warm unknown VRAM only `min_vram_gb == 0` on-disk models.
- Wiki (`ollama-router-product.md`, `ollama-router-load-share.md`) + `ollama-compat-proxy` + `routing-wlc` (`.cursor` and `.opencode`) + `openspec/config.yaml` context.
- Sensitivity: no pull/show/ps bodies, prompts, or job `detail` strings in logs or SQLite.
