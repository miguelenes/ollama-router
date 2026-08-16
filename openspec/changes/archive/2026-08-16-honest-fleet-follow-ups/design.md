## Context

See proposal.md (Why). `GET /api/tags` is already a healthy-node union with per-node `TagRecord`s. `GET /api/ps` is still `proxy_ranked` to one node; health only keeps loaded **names** plus a VRAM sum. `GET /api/version` is the same GENERIC passthrough. `POST /api/show` is GENERIC passthrough (holder coincidence, not a filter). `POST /api/pull` calls `orchestrator.ensure` and returns one JSON object after the job finishes. `bootstrap_desired_models` (and `bootstrap_probe_wait_seconds`, `bootstrap_require_*`) are parsed and unused. `NodeSnapshot` already has `gpu_util_pct` / `gpu_util_known` and `disk_available_gb`; `load_key` is a 4-tuple without GPU util; placement never reads disk. The warm-keeper calls `tier_models_for_vram(node.vram_gb())` and treats omitted free VRAM as `0`.

Axum 0.8 (`/tokio-rs/axum` `axum_v0_8_4`): return `(StatusCode, [(CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(stream))` and do not set `Content-Length`. Same `Body::from_stream` pattern as the reqwest-response example — flush chunks as the job advances; do not join the full body.

`POST /api/pull` placement class stays generate-class, not `RequestClass::Pull`.

## Goals / Non-Goals

**Goals:**

- Aggregate `GET /api/ps` from probe records (CLI-safe fields, per-node rows).
- Holder-only `POST /api/show` with 503 `model_missing`.
- NDJSON progress on HTTP pull without becoming a native single-node Hub-pull.
- Wire opt-in desired-tier bootstrap; warm-keeper stays disk-only but uses known VRAM/free (omitted ≠ `0`).
- Insert known-GPU-util into `load_key` after inflight and RAM pressure.
- Skip pull targets only when disk free is known and below the size estimate (target node’s tag `size`, else aggregated catalog).
- Serve `GET /api/version` from the router process (`CARGO_PKG_VERSION`, same as `/healthz`).

**Non-Goals:**

- Default `auto_pull_on_miss`, 404-on-miss, agent-down → unhealthy.
- Reversing 501 create/copy/push/blobs.
- Operator drain API, request queue, inference-path auth, public tunnels.
- Changing metrics gauges that publish `unwrap_or(0)` behind `*_known` flags.
- Pulling new models from the warm-keeper. Rewriting `rank_nodes` control flow beyond the sort key.
- `vram_free` / CPU util in the sort key. `DELETE /api/delete` NDJSON. Aggregating node Ollama versions.

## Decisions

### 1. Persist per-node ps records beside the loaded-name set

Keep `Cold.loaded_models: HashSet<String>` as the routing warmth source (`has_model_loaded`). Add `Cold.ps_records` keyed by normalized name (`PsRecord`: `digest`, `size`, `size_vram`, `details`, `expires_at`, `context_length`). Successful health `/api/ps` replaces both. Serde ignores extras; do not `deny_unknown_fields`.

Client `GET /api/ps` is a dedicated handler (same pattern as tags), not `proxy_ranked`. Emit **one row per (healthy node, loaded model)** with `details.router_node` so two cards both running `qwen3:8b` show two processes. Digest placeholder: SHA-256 of the normalized name, ≥12 chars, when the probe digest is missing or shorter (same CLI `[:12]` hazard as tags).

**Alternative considered:** Collapse to one row per name like tags. Rejected — `ps` is a process list; operators need which node is loaded.

**Alternative considered:** Re-fetch `/api/ps` on each client GET. Rejected — extra upstream load; health already polls.

### 2. Show uses the holder filter, still GENERIC

Before `proxy_ranked`, require `has_model` (healthy, non-draining). Empty set → 503 `model_missing` (not native 404). Classify stays GENERIC so LARGE VRAM gates do not hide a holder that can still `show` a 70B blob. Prefer warm via existing `load_key`. Do not log the show body.

**Alternative considered:** Rank GENERIC without a holder filter (today). Rejected — CLI show against the wrong node is a native 404.

**Alternative considered:** Classify show as the model’s generate class. Rejected — would 503 `insufficient_capacity` on unknown-VRAM holders that still have the files.

### 3. HTTP pull: `start_ensure` + NDJSON stream, job keeps running if the client drops

Replace `ensure()` (wait, then JSON) on `POST /api/pull` with `start_ensure` (default placement, `include_unhealthy: false`) and `Body::from_stream` over job-status notifications. Map target counts to `total` / `completed` (node×model pairs, not fake Hub layer bytes). Status strings are router-owned (`pulling`, `success`); never copy upstream NDJSON or `JobTarget.detail` into the stream or SQLite. Terminal success → final `{"status":"success"}`. Partial failure → `{"error":"… [reason: …]"}` and no success line (HTTP 502 after the stream has begun is acceptable; 503 + Retry-After only for `NoPlacementTargets` **before** headers). Client disconnect cancels the stream, **not** the job.

Admin `/router/v1/models/ensure` stays JSON. `auto_pull_on_miss` wait path stays internal.

**Alternative considered:** Passthrough native `/api/pull` to one node. Rejected — honest fleet is a placement job; one-node Hub-pull leaves the rest empty.

**Alternative considered:** Keep JSON `{status: success}` and document that the CLI cannot watch progress. Rejected — in-scope CLI consume.

### 4. Bootstrap uses generate-class placement + known `min_vram_gb`, in the background

When `bootstrap_desired_models` is true, spawn after bind (and on reload): wait up to `bootstrap_probe_wait_seconds` for health, then `start_ensure_targets` per desired model. Targets = `placement_eligible_node_ids` intersect nodes whose **known** VRAM ≥ that tier’s `min_vram_gb`. Unknown VRAM is eligible only when generate-class admits unknown **and** `min_vram_gb == 0`. `bootstrap_require_capacity` (default true) keeps those static VRAM gates; `bootstrap_require_ram_headroom` (default true) passes `avoid_ram_pressure: true`. Default `bootstrap_desired_models` stays false. Coalesce with in-flight pull jobs. Do not use `tier_models_for_vram(node.vram_gb())` for LARGE/MEDIUM — that helper treats omitted VRAM as 0.

Warm-keeper stays `has_model` && !`has_model_loaded` only (no pull). Replace `vram_gb()` / `Some(0)` free fallbacks: known VRAM ≥ `min_vram_gb`; unknown VRAM only `min_vram_gb == 0`; skip for free VRAM only when `vram_free_gb` is `Some` and below `model_warm_min_free_vram_gb`. Do not change metrics gauges.

**Alternative considered:** Fail serve if bootstrap jobs fail. Rejected — must not block listen.

**Alternative considered:** Agent-down nodes unhealthy so bootstrap “sees” GPUs. Rejected — breaks tags listing and soft-fail.

**Alternative considered:** Warm every on-disk desired-tier model when VRAM is unknown (treat unknown as a big GPU). Rejected — same as placing LARGE on unknown.

### 5. GPU util is a fifth `load_key` term, not a hard filter

`load_key` → `(inflight/cap, ram pressure, gpu_util_key, class preference, warm+ram ratio)`. Do not change `rank_nodes` filters. `gpu_util_key` bands (busy threshold **50%**, no new YAML knob):

- known util `< 50` → band 0 + `pct/1000` (10% before 40%)
- unknown / `gpu_util_known == false` → band 1
- known util `≥ 50` → band 2 + `pct/1000`

Inflight still dominates (tuple position 1). Do not treat `gpu_util_pct.unwrap_or(0)` as idle on the rank path. Leave Prometheus `ollama_router_node_gpu_utilization_pct` + `gpu_util_known` as they are.

**Alternative considered:** Sort unknown as 0. Rejected — silent GPU looks idler than a 10% card.

**Alternative considered:** GPU util before RAM pressure. Rejected — OOM-risk should lose before a util tie-break.

### 6. Disk skip is known-only, catalog `size` only, placement only

At placement resolution **and** at run, skip when `disk_available_gb` is `Some` and the pull size estimate is known and `disk_available_gb < estimate_gib`. Estimate source: that **target node’s** tags-probe `size` when present and `> 0`; else the aggregated catalog `size` for the model when present and `> 0`; else do not skip (GiB = bytes / 1024³). Missing/`0` size or `None` disk → do not skip. New `TargetStatus::SkippedDisk` (`skipped_disk`); treat as success-like like other skips. Do not put disk in `load_key`. Do not persist skip `detail` text.

**Alternative considered:** Treat missing disk as 0 and skip everyone when the agent is down. Rejected — same unknown=0 lie as VRAM; agent soft-fail.

**Alternative considered:** Skip using a class floor when `size` is absent. Rejected — proposal is probe `size` when present; otherwise fail open.

### 7. Version is the router crate, not a ranked Ollama

Dedicated `GET /api/version` handler returns `{"version": env!("CARGO_PKG_VERSION")}` — the same string as `/healthz`. Do not `proxy_ranked`. Native shape only (no extra fields) so strict clients keep parsing. Not idle.

**Alternative considered:** Forward to one healthy node so the string matches Ollama. Rejected — same single-daemon lie as old `/api/ps`.

**Alternative considered:** Min/max union of node versions. Rejected — not a native field; clients expect one `version` string.

## Risks / Trade-offs

- [CLI `ps` prints duplicate names] → Intentional per-node rows; wiki notes `details.router_node`.
- [CLI pull progress bar is node-count, not layer bytes] → Honest; status text still moves.
- [Bootstrap storms Hub on a cold fleet] → Default false; coalesce jobs; `max_pulls_per_node` unchanged.
- [50% GPU band is a magic number] → Only mixed known/unknown needs a band; both-known still orders by pct. Tune later with a knob if needed.
- [Stale ps records] → Next probe replaces; empty probe clears loaded set.
- [Clients expecting a node Ollama semver on `/api/version`] → Wiki: this is the proxy version; node builds stay on admin/node-agent.
- [Unknown-VRAM nodes start warming min_vram_gb 0 models] → Intentional; they were skipped by fake 0 GiB free.
- [Rank MODIFIED retains free-VRAM scenarios already on main] → `openspec/specs` was synced with `vram-free-ranking-and-delete-streams` while this change is still open. The Rank/Class MODIFIED blocks copy that composed text so archive cannot drop those scenarios. This change still does **not** implement free-VRAM ranking or delete streams (tasks stay GPU-util only; that work stays in the later change).

## Migration Plan

1. Deploy router binary. No fleet.yaml or SQLite schema bump required beyond storing `skipped_disk` on new rows (`FromStr` ignores unknown old values).
2. Until the first `/api/ps` probe, aggregated ps is empty (same as no loaded models today).
3. Operators who want desired models on new GPUs set `bootstrap_desired_models: true`.
4. Rollback: previous binary passthrough `ps`/`show`/`version`, JSON pull, unused bootstrap knob, GPU util ignored, warm encodes unknown free as 0.

## Open Questions

None that change the specs.
