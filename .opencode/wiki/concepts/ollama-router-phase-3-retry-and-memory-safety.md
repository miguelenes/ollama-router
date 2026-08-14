---
title: Ollama Router Phase 3 Retry and Memory Safety
tags: [ollama, router, retry, affinity, telemetry, warm-keeper]
sourceRefs:
  - crates/ollama-router/src/proxy
  - crates/ollama-router-core/src/routing
  - crates/ollama-router/src/http
lastReviewed: 2026-08-14
---

# Phase 3 retry and memory safety

Pre-response upstream handling is bounded and selection-aware.

- `IncrementalCollector` limits an unterminated NDJSON or SSE frame to **1 MiB**.
  SSE mode is selected from upstream `Content-Type: text/event-stream`. It
  discards telemetry parsing until the next newline but never changes chunks
  forwarded by the proxy.
- Sticky affinity may promote its owner only when its **full routing load key**
  exactly equals the best eligible candidate. It cannot override lower
  utilization, capacity preference, warmth, RAM pressure, or RAM bias.
- Pre-response retries rerank after each retryable failure and exclude all
  attempted node IDs. **No retry once a stream has begun.**
- Nonretryable pre-response upstream errors become **502** responses instead of
  leaking framework exceptions. Body shape follows the request path (Ollama
  string on `/api/*`, OpenAI envelope on `/v1/*`).
- The warm keeper occupies the target node's inflight counter while warming,
  releases it in `Drop`/`finally`, checks HTTP status before success logging,
  and **must not** call `inflight_inc` (which would reset idle activity).

See [[concepts/ollama-router-load-share]] and
[[concepts/ollama-router-idle-scale-down]].
