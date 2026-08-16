## Context

See proposal.md (Why) and the four delta specs. WLC ranking, holders-only 503, streaming retry, and placement-to-all-eligible already live in `routing/rank.rs`, `proxy/`, and `place.rs`. `Capacity::vram_gb()` / `gpus()` still map `None` → 0, so those paths treat a silent GPU as a CPU. `classify` does not read `TagRecord.details.parameter_size`. `POST /api/pull` uses `placement_class` = classify(`/api/generate`, model), not `RequestClass::Pull`.

## Goals / Non-Goals

**Goals:**

- Routing and `static_capacity_fits` read **optional** VRAM/GPU.
- Lock existing inference/placement behavior in specs; only add tests where scenarios are missing.
- Class hint from catalog `parameter_size` when `:Nb` is absent.
- Static capacity on `fleet.local.yaml`; mock GPU `gpus: 1`.

**Non-Goals:**

- Reimplement WLC or the orchestrator.
- Health / agent-down unhealthy / `public_url_blocked`.
- Pull NDJSON, default auto-pull, 404-on-miss, fleet `/api/ps`, GPU-util sort key.
- Prefix-match `minicpm-v*` (use `parameter_size`).

## Decisions

### 1. Known vs zero is `Option` on the rank/placement path

YAML `0` / `gpus: 0` remains a measured CPU. Omitted stays `None`. Add routing accessors; do not blindly change metrics `vram_gb()` callers.

**Alternative considered:** Fail-unhealthy on unknown. Rejected — agent soft-fail, tags must still list the node.

**Alternative considered:** Distinct inflight cap for unknown. Rejected — spec allows the lowest-tier number; gates, SMALL bands, and EMBED/MEDIUM not treating omitted as `0` are the bug.

### 2. MEDIUM/LARGE unknown does not fit ranking **or** placement

`vram_fits` / `static_capacity_fits`: MEDIUM/LARGE need `Some(vram)` meeting existing thresholds. EMBED/SMALL/GENERIC stay admissible. `RequestClass::Pull` is unused by HTTP pull targeting; `start_ensure` / `placement_eligible_node_ids` use generate class, so LARGE jobs skip unknown VRAM and known CPUs. Default HTTP ensure uses `include_unhealthy: false`; `#all` may widen.

### 3. Preference uses known VRAM/GPU; utilization still first

GPU-first, CPU as overflow: do not invert SMALL/EMBED toward CPU-home. Utilization (`load_key` position 1) still dominates — an idle GPU takes SMALL/embed even if a CPU holder is free. SMALL bands: known GPU (`gpus >= 1`), then unknown GPU count, then known CPU (`Some(0)`). EMBED and MEDIUM compare **known** VRAM only — omitted MUST NOT sort as `0` (would win as the “smallest GPU”). EMBED still prefers lower known VRAM, so a measured CPU (`Some(0)`) can win an **equal-load tie** against an 8 GiB GPU; that is existing WLC, not CPU-home. LARGE unknown is already excluded by the VRAM gate. Existing proptest (utilization beats preference; saturated never selected) stays.

### 4. Class hint from the catalog

`size_hint_b` after `:Nb` fails (after embed markers and exact small bases). Proxy and `placement_class` fill from aggregated-tag winner `details.parameter_size`. No `/api/show`. Do not log `details`.

### 5. Specs for existing WLC/placement are verify-first

Do not rewrite `rank_nodes` sort key. Add tests only for new unknown-VRAM and `parameter_size` scenarios, plus placement tests that LARGE ensure omits unknown VRAM and known CPU, and that MEDIUM default placement omits a known CPU. Keep current stream/retry tests.

### 6. Local-dev static capacity is GitOps

`deploy/fleet.local.yaml` sets `capacity.vram_gb` / `gpus` (comment: `0`/`0` = known CPU; omit = unknown). Mock `deploy/fleet.yaml` GPU row gets `gpus: 1` if missing.

## Risks / Trade-offs

- [LARGE 503 / skipped pulls on unknown-only fleets] → Static `capacity` on GPU rows; local-dev example.
- [Wrong placeholder GiB] → Comment to match host; agent min() when discovery is positive.
- [Broader spec vs small apply] → Tasks implement only unknown≠0, class hint, inventory, missing tests, docs.

## Migration Plan

1. Apply ranking/class/placement-gate tests; more honest 503s and skipped LARGE pulls on unknown GPUs until fleet.yaml declares VRAM.
2. Operators add `capacity` on GPU nodes.
3. Rollback: previous binary collapses `None` → 0; no durable format.

## Open Questions

None that change the specs.
