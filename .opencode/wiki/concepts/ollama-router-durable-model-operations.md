---
title: Durable Ollama router model operations
tags: [ollama-router, sqlite, operations, recovery]
sourceRefs:
  - crates/ollama-router-core/src/jobs
  - crates/ollama-router-core/src/config
lastReviewed: 2026-08-16
---

# Durable Ollama router model operations

The pull/delete orchestrator in `crates/ollama-router-core/src/jobs/` persists
operation metadata to SQLite at `/var/lib/ollama-router/model-operations.sqlite3`
by default so records survive process restarts.

Only identity and lifecycle metadata is retained: operation ID, kind, status,
timestamps, normalized models/nodes, and per-target status. Upstream response
bodies and per-target `detail` strings are memory-only — they can contain
sensitive provider error text.

On construction, persisted running operations rebuild the in-flight dedupe map.
`recover_incomplete_jobs` resumes them, probing each target's live `/api/tags`
before pull/delete work. The next ensure/delete also starts recovery lazily, so
an identical request receives the existing job rather than duplicating work.

When `policy.auto_pull_on_miss` is true, generate/chat/embed `model_missing`
calls `PullOrchestrator::start_ensure` (fire-and-forget, same stampede key as
admin ensure). Native `POST /api/pull` and `DELETE /api/delete` stream fleet-job
NDJSON (`application/x-ndjson`) via `start_ensure` / `start_delete` + a job
watcher (`total`/`completed` from targets; final `success`). Already-absent
delete is a success stream (`NoTargetNodes`). Admin ensure/delete stay JSON.
SQLite still stores metadata only — never pull/delete error text or bodies.

Terminal jobs remain queryable across restarts and use TTL/count retention.
Pruning deletes both the in-memory view and the SQLite row. Call
`recover_incomplete_jobs` during Axum lifespan startup so recovery runs before
the first operation.

## Cancel

Admin `POST /router/v1/jobs/{id}/cancel` (fail-closed bearer) marks every
incomplete target `TargetStatus::Cancelled` (failure-like job summary), stamps
`finished_at`, persists, and notifies watchers. Unknown id → 404; already
terminal → 409 unchanged. Dispatch skips non-incomplete targets; `set_target`
refuses to overwrite a terminal status. The SQLite status string is additive;
unknown status on read parses as failed.

## Related

- [[concepts/ollama-router-product]]
