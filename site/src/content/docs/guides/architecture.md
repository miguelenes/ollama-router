---
title: Architecture
description: The honest-fleet proxy contract — how the router differs from a single Ollama daemon.
sidebar:
  order: 6
---

The router is an **honest fleet proxy**, not a fake single daemon. Every
deviation from a single Ollama is deliberate and documented here.

## Contract deviations

| Surface | A single Ollama | The router |
| --- | --- | --- |
| `GET /api/tags` | models on this machine | **Union** of holders across healthy, non-draining nodes |
| inference | any local model | Only nodes that **already have** the model |
| `POST /api/pull` | Hub pull on this daemon | **Placement job** streaming NDJSON progress across the fleet |
| model miss | 404 | **503 `model_missing`** |
| `create`/`copy`/`push`/`blobs` | supported | **501** (not implemented on the fleet) |
| `auto_pull_on_miss` | — | stays **false** by default |
| `POST /api/show` | local model info | From the model's **holder only** (503 `model_missing` if none) |
| `GET /api/version` | Ollama build version | Router-owned version (not a ranked Ollama's) |
| `GET /api/ps` | loaded models here | Union of process lists — **one row per loaded node** |

## Ranking (utilization WLC)

Utilization dominates. Nodes pass **hard filters** first: healthy, label
match, model on disk, capacity fits (static VRAM + live headroom +
reservations), and `inflight < effective_max`. Saturation is a hard filter —
all saturated means **503 `all_nodes_saturated`**. Survivors sort by
`inflight / base_cap`, then RAM pressure, then **known** GPU utilization
(unknown is middle, not 0), then class preference:

| Class | Preference |
| --- | --- |
| EMBED | lower **known** `vram_gb`; unknown after known |
| SMALL | known GPU → unknown → known CPU |
| MEDIUM | lower **known** `vram_gb`; unknown is never 0 |
| LARGE | higher known VRAM; unknown hard-gated |
| GENERIC | known `vram_gb` |

GPU-first: CPU is overflow when the GPU is saturated or busier. Omitted
VRAM/GPU is *unknown*, not a measured CPU.

## Replica and state

- **One router replica.** No Redis, no HA replicas.
- Streams are forwarded as they arrive: NDJSON for native Ollama, SSE for the
  OpenAI surface. Retries target another node **only before the first byte**.
- `/healthz`, `/readyz`, and `/metrics` are unauthenticated. Prometheus
  metrics are not labeled by model name. The admin API
  (`/router/v1/*`) requires `Authorization: Bearer $OLLAMA_ROUTER_ADMIN_TOKEN`;
  an unset token disables it with **403** — no default secret.
- The operator console lives at `/router/ui`.
