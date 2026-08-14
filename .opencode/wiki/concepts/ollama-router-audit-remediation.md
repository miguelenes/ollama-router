---
title: Ollama router audit remediation
tags: [ollama-router, reliability, security, autoscaling, operations]
sourceRefs:
  - crates/ollama-router-core
  - crates/ollama-router/src/proxy
  - crates/ollama-router-verda
lastReviewed: 2026-08-14
---

# Ollama router audit remediation

Preserve these five remediations in the Rust rewrite:

1. **Fleet lifecycle correctness.** The registry emits dynamic node events.
   Health probing starts and cancels with every node lifecycle change. Scale-out
   creates additional Verda instances rather than re-adopting the first.
   Failed teardown retains ownership for retry.
2. **Coordinated cloud capacity.** A no-capacity request calls **Verda only**
   (the sole provider). Respect `auto_scale_max_instances`. Coalesce in-flight
   `ensure`. Never fan one demand event to multiple clouds.
3. **Safe proxy data plane.** Cap request bodies before buffering. Retries
   re-rank without already-failed nodes. Incomplete NDJSON telemetry is bounded.
   Upstream failures use gateway-shaped responses. Warm requests occupy inflight
   without counting as client demand.
4. **Fail-closed operations.** Admin auth has no implicit default. Environment
   parsing and credential references fail closed. FleetState recovers from a
   valid backup or surfaces corruption rather than silently forgetting ownership.
5. **Restart-safe model operations.** Pull/delete metadata and dedupe state
   persist in SQLite; incomplete work reconciles from live `/api/tags` on startup.

These preserve env-first fleet config, tunnel/loopback-only Verda routing
(self-hosted zrok private share), and router-owned idle teardown. Related:
[[concepts/ollama-router-product]],
[[concepts/ollama-router-idle-scale-down]],
[[concepts/ollama-router-durable-model-operations]],
[[concepts/ollama-router-phase-3-retry-and-memory-safety]].
