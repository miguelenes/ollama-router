## Context

See proposal.md (Why). Today: the proxy already parses the generate/chat body for `model`; `keep_alive` is ignored, so `ollama stop` ranks and forwards to one holder and counts as idle activity. `Registry.draining` is set only by Verda teardown and inventory-remove, and `should_remove_permanent` drops a draining permanent node at zero inflight — so operator cordon must not reuse that flag. `NodeSnapshot.cpu_usage_pct: Option<f64>` is already populated from the agent (`Some` = known) but unused in `load_key`. Jobs have a process-wide shutdown token but no per-job cancel; `Job` status summarizes from target statuses. Saturation is an immediate 503 from `rank_nodes`.

## Goals / Non-Goals

**Goals:**

- Fleet unload fan-out for unload-intent generate/chat, quiet for idle.
- Operator cordon that cannot be confused with inventory-remove draining.
- CPU-util soft term in `load_key` with the same known/unknown discipline as GPU util and free VRAM.
- Per-job cancel with additive persistence.
- Opt-in bounded saturation wait, default off.

**Non-Goals:**

- Cross-request fairness or a persistent admission queue.
- Persisting cordon state across router restarts.
- Agent-side changes (all signals used already exist in `CapacityReport`).
- Cancelling in-flight upstream HTTP mid-request (targets finish or fail on their own; their result no longer changes a cancelled job).

## Decisions

### 1. Unload is a proxy fan-out, not a job and not a passthrough

Detect unload-intent before ranking, from the already-parsed body: `keep_alive` parses to `<= 0` (number or duration string `"0s"`) **and** `prompt` (generate) / `messages` (chat) is absent or empty. Targets are the ps-union loaded holders (healthy, non-draining, model in `PsRecord`s). Fan out in parallel with the existing upstream client and default timeout; each target gets the native unload body for its endpoint. All-success (or zero targets) → router-owned `{"model", "created_at", "response": "", "done": true, "done_reason": "unload"}`; any failure → 502 router-owned error, no upstream text. No `inflight_inc`, no `last_client_request_at`, no idle reservation.

**Alternative considered:** durable SQLite job like pull/delete. Rejected — unload is fast, idempotent, and worthless after restart; a job adds schema and stream plumbing for nothing.

**Alternative considered:** rank-and-forward to one node (status quo). Rejected — leaves the model loaded elsewhere; dishonest ps afterwards.

### 2. Operator cordon is a separate bit from `draining`

Add `cordoned: AtomicBool` to registry nodes, set only by the admin drain/undrain routes. Ranking, placement targets, bootstrap, and warm-keeper exclude `draining || cordoned`; health/capacity probes ignore it. `should_remove_permanent` and Verda teardown keep reading only `draining`, so a cordoned fleet.yaml host is never dropped or destroyed. Snapshot gains `cordoned`; the admin nodes listing shows it and `ollama_router_node_draining` reports `draining || cordoned` (the operator-facing meaning: "not taking new work").

**Alternative considered:** reuse `draining`. Rejected — reload reconcile drops permanent draining nodes at zero inflight; a maintenance cordon would remove the node from the registry.

**Alternative considered:** persist cordon in FleetState. Rejected for now — single replica, restart clears cordon; documented risk, avoids schema churn.

### 3. CPU util is the seventh `load_key` field

`cpu_util_key(node)` mirrors `gpu_util_key`: `Some(pct)` below the busy band → `pct / 1000`; busy → `2.0 + pct / 1000`; `None` → `1.0` (middle). Busy threshold `CPU_UTIL_BUSY_PCT = 80.0` (no YAML knob) — CPU boxes legitimately run hotter than the 50% GPU band; inflight/cap still dominates. Tuple becomes `(inflight, pressure, gpu_util, vram_free, cpu_util, preference, warm+ram)`. `Option` is the known flag; no `unwrap_or(0.0)` on the rank path. Metrics gauges unchanged.

**Alternative considered:** reuse the 50% GPU busy band. Rejected — half the CPU fleet would sit in the busy band under normal load, collapsing the signal.

### 4. Cancel marks targets, dispatch checks, terminal writes are ignored

New `TargetStatus::Cancelled` (failure-like: job summary MUST NOT read success). `PullOrchestrator::cancel_job(&JobId)`: under the lock — unknown → `NotFound`; terminal → `Conflict`; else set every incomplete target to `Cancelled`, stamp `finished_at`, persist, notify watchers (existing NDJSON stream already emits the error line for non-success terminal jobs). The dispatch loop skips targets that are no longer incomplete, and `set_target` refuses to overwrite a terminal target status, so a late in-flight result cannot resurrect a cancelled job. SQLite stores the new status string additively; on read, an unrecognized status parses as failed so an old binary rolling back does not crash.

**Alternative considered:** new `JobStatus::Cancelled` variant. Rejected — job status is derived from targets everywhere (summary, streams, admin); a parallel top-level state invites drift. Cancelled targets making the job `Failed` is observably "terminal non-success", which is what the spec requires.

### 5. Saturation wait loops on a registry `Notify`

Registry gains `Arc<tokio::sync::Notify>`; `inflight_dec` calls `notify_waiters()` (wakes all current waiters, stores no permit — losers re-arm). Proxy: on `RoutingError::Saturated` with `policy.saturation_wait_seconds > 0`, loop `{ re-rank; if admissible → forward; else notified().await bounded by the remaining deadline }` via `tokio::time::timeout`. Deadline expiry → existing 503 + `saturated_retry_after_seconds`. Dropping the request future (client disconnect) drops the waiter. The body was already read (existing cap); nothing else is buffered.

**Alternative considered:** poll every 100 ms. Rejected — busy-wakeups and up to 100 ms added latency for a one-line notify on the release path.

**Alternative considered:** queue with FIFO fairness. Rejected (non-goal) — `notify_waiters` + re-rank is racy between waiters, but requests are interchangeable and the bound is short.

## Risks / Trade-offs

- [Cordon lost on restart] → documented; operator re-drains after a restart (single replica, rare).
- [Unload misses a node loaded since the last probe] → idempotent; rerun after the next ps probe. Probe freshness is the same trust ps already has.
- [`keep_alive` duration strings vary (`0`, `0s`, `-1m`)] → parse number or Go-style duration; only non-positive triggers unload; anything unparseable falls through to normal inference.
- [Stricter sticky equality with a 7-field key] → equality only tightens; sticky promotion already requires exact key equality.
- [Old binary reads a `cancelled` SQLite row after rollback] → status parse falls back to failed-like terminal, never a crash (verify in store tests).
- [Waiting requests hold memory under burst] → bounded by existing body cap and the wait deadline; default off.

## Migration Plan

1. Deploy the binary; all behavior changes are additive or opt-in (`saturation_wait_seconds` default 0).
2. No fleet.yaml or agent changes; SQLite gains only a new target-status string.
3. Rollback: previous binary ignores cordon/cancel routes (404), treats unload-intent as normal generate again, and parses unknown target status as failed.

## Open Questions

None that change the specs.
