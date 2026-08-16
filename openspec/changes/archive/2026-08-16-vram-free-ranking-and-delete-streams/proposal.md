## Why

Holders with equal inflight (and, after `honest-fleet-follow-ups`, equal GPU util) still look identical while one GPU is packed and the other has headroom — ranking never reads `vram_free_gb`. `DELETE /api/delete` still waits for a JSON blob after the fleet job, so `ollama rm` against the router has no progress while pull is gaining an NDJSON stream.

## What Changes

This change **keeps** the honest-fleet contract: list = union of holders; infer = holders only; pull/delete = fleet jobs (not native mutates through one node); miss = 503 `model_missing`; create/copy/push/blobs stay 501; `auto_pull_on_miss` stays default **false**.

Apply **after** `honest-fleet-follow-ups` (GPU-util sort term and pull NDJSON watcher).

- Ranking **applies** a **soft** known-free-VRAM bias after inflight, RAM pressure, and known GPU util. Higher known free ranks first. Unknown free MUST NOT sort as `0` (full). Inflight and GPU-util gaps still dominate. Unknown VRAM vs measured CPU is unchanged. This is **not** a new hard filter (agent-down stays healthy).
- `DELETE /api/delete` stays a fleet delete job and **streams NDJSON progress** the official CLI can consume (same envelope as HTTP pull: `status`, `total` / `completed` when known, final `success`). It does not become a single-node native delete. No-target (already absent) stays success. Client drop does not cancel the job.
- Docs/wiki + `ollama-compat-proxy` / `routing-wlc`: known free VRAM is a rank tie-break; delete streams like pull.

Not in this change: CPU util in the sort key, making unknown free a hard 503, default `auto_pull_on_miss`, 404-on-miss, agent-down → unhealthy, reversing 501 mutates, operator drain API, public tunnels, Thunder/RunPod, native Hub-pull through one node.

## Capabilities

### New Capabilities

- `api-delete`: `DELETE /api/delete` streams fleet-job NDJSON progress while remaining a multi-node delete (not a native single-node mutate).

### Modified Capabilities

- `inference-routing`: Known free VRAM biases ranking after inflight, RAM pressure, and GPU util; it MUST NOT override those gaps.
- `size-load-routing`: Unknown free VRAM is unknown, not zero (a full GPU).

## Impact

- `crates/ollama-router-core/src/routing/rank.rs` — `vram_free` term after GPU util; `vram_free_known` false must not encode as 0. Do not change metrics `vram_free_gb.unwrap_or(0)` callers gated by `vram_free_known`.
- `crates/ollama-router/src/proxy/` — NDJSON delete stream (reuse pull job watcher); do not log bodies.
- `crates/ollama-router-core/src/jobs/` — delete progress to the stream (metadata only in SQLite; no `detail` strings).
- Tests: equal load → more known free wins; known 8 GiB free beats unknown; unknown beats known 0 free; inflight/GPU-util still dominate; delete NDJSON success including already-absent; two holders both targeted; delete does not set `last_client_request_at`.
- Wiki (`ollama-router-load-share.md`, `ollama-router-durable-model-operations.md`) + `ollama-compat-proxy` + `routing-wlc` (`.cursor` and `.opencode`) + `openspec/config.yaml` (known free VRAM in the sort key; CPU util stays an A-feature non-goal).
- Sensitivity: no delete bodies, prompts, or job `detail` strings in logs or SQLite.
