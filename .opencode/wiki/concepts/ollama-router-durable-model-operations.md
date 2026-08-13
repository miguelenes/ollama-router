---
title: Durable Ollama router model operations
tags: [ollama-router, sqlite, operations, recovery]
sourceRefs:
  - crates/ollama-router-core/src/jobs
  - crates/ollama-router-core/src/config
lastReviewed: 2026-08-13
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

Terminal jobs remain queryable across restarts and use TTL/count retention.
Pruning deletes both the in-memory view and the SQLite row. Call
`recover_incomplete_jobs` during Axum lifespan startup so recovery runs before
the first operation.

## Related

- [[concepts/ollama-router-product]]
