## Context

See proposal.md (Why). Apply **after** `honest-fleet-follow-ups` so `load_key` already has a GPU-util term and HTTP pull already streams job NDJSON via Axum `Body::from_stream`. Today `NodeSnapshot.vram_free_gb` / `vram_free_known` feed Prometheus only (`unwrap_or(0)` gated by the known flag). `capacity_preference` EMBED adds +100 when `known_vram - loaded < 2`, which is not a general free-VRAM sort term. `DELETE /api/delete` calls `orchestrator.delete` and returns one JSON object after `wait_job`. `NoTargetNodes` is already HTTP 200 `{status: success}`.

Axum 0.8 (`/tokio-rs/axum` `axum_v0_8_4`): `(StatusCode, [(CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(stream))` — no `Content-Length`, no join of the full body. Same watcher as pull.

## Goals / Non-Goals

**Goals:**

- Soft known-free-VRAM term after inflight, RAM pressure, and GPU util.
- Unknown free is a middle band, not `0` GiB full and not infinite headroom.
- Stream `DELETE /api/delete` like HTTP pull; keep already-absent success.

**Non-Goals:**

- CPU util in the sort key.
- Hard-fail / 503 when free is unknown (agent soft-fail).
- Changing EMBED’s existing +100 tight-headroom preference (it stays; free-VRAM term runs *before* class preference).
- Native single-node delete. Admin delete JSON. Pull stream (follow-ups).

## Decisions

### 1. Free VRAM is the sixth `load_key` field, not a hard filter

After follow-ups: `(inflight/cap, ram pressure, gpu_util_key, vram_free_key, class preference, warm+ram ratio)`. Do not add a `rank_nodes` filter on `vram_free_known`. `vram_free_key` bands (tight threshold **2 GiB**, same number EMBED already uses; no new YAML knob):

- known free `>= 2` → band 0 + `(-free / 1000)` (8 GiB before 4 GiB)
- unknown / `vram_free_known == false` → band 1
- known free `< 2` → band 2 + `(-free / 1000)` (0.5 GiB before 0)

Inflight and GPU util still dominate (tuple positions 1 and 3). Do not treat `vram_free_gb.unwrap_or(0)` as full on the rank path. Leave Prometheus gauges as they are.

**Alternative considered:** Encode unknown as 0. Rejected — silent GPU looks packed and never wins a tie.

**Alternative considered:** Encode unknown as +∞ free. Rejected — silent GPU looks emptier than an 8 GiB-free card.

**Alternative considered:** 503 `insufficient_capacity` when free is unknown. Rejected — agent-down would hide holders (same as making agent-down unhealthy).

**Alternative considered:** Free VRAM before GPU util. Rejected — a busy card with leftover VRAM should still lose to an idle card.

### 2. Delete reuses the pull job stream, not a native mutate

Replace `delete()` wait-then-JSON on `DELETE /api/delete` with `start_delete` (healthy, non-draining holders; `include_unhealthy: false`) + the same NDJSON watcher as pull (`total`/`completed` = node×model pairs). Router-owned status strings; never copy `JobTarget.detail`. Terminal success (including `NoTargetNodes` / already absent) → `{"status":"success"}`. Partial failure → error object, no success. Client disconnect cancels the stream, **not** the job. Admin `/router/v1/models/delete` stays JSON.

**Alternative considered:** Passthrough native `/api/delete` to one node. Rejected — leaves the model on every other holder.

**Alternative considered:** Keep JSON for delete because `ollama rm` has no progress bar. Rejected — in-scope CLI stream; same envelope as pull.

## Risks / Trade-offs

- [Two open changes both MODIFY `Rank among holders`] → Apply and archive `honest-fleet-follow-ups` first; this delta is vs that sort key.
- [2 GiB band is a magic number] → Matches existing EMBED tight-headroom; both-known still orders by GiB.
- [Already-absent delete is a success stream, not 404] → Existing orchestrator contract; CLI `rm` is idempotent.

## Migration Plan

1. Land `honest-fleet-follow-ups` (GPU util + pull stream).
2. Deploy this binary. No fleet.yaml or SQLite schema bump.
3. Rollback: previous binary ignores free VRAM in rank; delete is JSON wait.

## Open Questions

None that change the specs.
