---
title: Status codes
description: Every 503 reason code the router emits, and the Retry-After contract.
sidebar:
  order: 1
---

The router answers failures with **503 + `Retry-After`** on every proxied
path. The body shape follows the request: Ollama-style strings on `/api/*`,
OpenAI envelopes (`{"error": {...}}`) on `/v1/*`.

| Reason code | Meaning |
| --- | --- |
| `no_healthy` | No healthy node can serve the request. |
| `saturated` | The best candidates are at their inflight cap. |
| `all_nodes_saturated` | Every eligible node is saturated. |
| `model_missing` | No healthy node holds the model (inference or `/api/show`). |
| `insufficient_capacity` | MEDIUM/LARGE request but no node with known sufficient VRAM (unknown is not enough). |
| `public_url_blocked` | A node's URL is a public tunnel or public `:11434` — it is never healthy. |
| `pull_enqueued` | A capacity miss enqueued an async fleet pull (only when `auto_pull_on_miss` is enabled). |

## Related statuses

- **501** — create/copy/push/blobs are not implemented on the fleet.
- **403** — admin API without (or with the wrong) bearer token; fail-closed
  when `OLLAMA_ROUTER_ADMIN_TOKEN` is unset.
- **502** — upstream failure before the first response byte that is not
  retryable; retries target another node **only before the first byte**.

## Why Retry-After

The router never blocks a client while cloud capacity provisions. Misses and
saturation return immediately with a retry hint so clients back off while the
fleet catches up (async create/ensure, auto-pull, warm-up).
