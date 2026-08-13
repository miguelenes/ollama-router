---
name: ollama-compat-proxy
description: Implements the Ollama-compatible proxy surface for this fleet router — generate/chat/embed, /api/embeddings rewrite, aggregated /api/tags and /v1/models, fleet pull/delete, and 503+Retry-After on capacity miss. Use when adding or changing HTTP routes, proxy streaming, admin ensure/delete, or Ollama protocol compatibility.
---

# Ollama-compatible proxy

Read `.opencode/wiki/concepts/ollama-router-product.md` and
`.opencode/wiki/concepts/ollama-router-phase-3-retry-and-memory-safety.md`.
Code lives under `crates/ollama-router/src/proxy/` and `.../http/`.

## Surface

| Path | Behavior |
|------|----------|
| `POST /api/generate`, `/api/chat` | Stream NDJSON. `inflight_inc` (idle activity). |
| `POST /api/embed` | Stream/JSON. `inflight_inc`. |
| `POST /api/embeddings` | Rewrite path to `/api/embed` (Ollama ≤0.32). Then same as embed. |
| `GET /api/tags` | Aggregated **union** of healthy nodes' tags. Not a single-node passthrough. |
| `GET /v1/models` | Same union in OpenAI list format (`id` / `object` / `created: 0` / `owned_by: library`). |
| `POST /api/pull`, `/api/delete` | Always fleet orchestrator (stub → 503 until jobs land). Prefer admin. |
| `GET /healthz` | Process up. |
| `GET /readyz` | Healthy capacity (optional embedding-model gate). |
| `GET /metrics` | Prometheus. Count-only model gauges (`aggregated_models`, `node_models`) plus `discovery_total`. Never a model-name label. Grafana Models row joins agent `ollama_up` / `ollama_models`. |
| `/router/v1/*` | Admin bearer. Unset token → 403. |

## Capacity miss

Map `no_nodes` / `no_healthy` / `capacity` / `ram` / `ram_pressure` /
`saturated` to Ollama-shaped **503** JSON and set `Retry-After`. Kick coalesced
async Verda demand-scale (`create_additional`) for those reasons. Never block
the client on provision.

`model_missing` has **no** Retry-After when `auto_pull_on_miss` is false (default).
When the flag is on, the proxy enqueues `start_ensure(Placement)` on
`placement_eligible_node_ids` only (static VRAM/class — LARGE never lands on
CPU). Empty placement → `insufficient_capacity` + provision Retry-After + demand
scale. Else 503 JSON `reason=pull_enqueued` + `Retry-After` from
`pull_miss_retry_after_seconds`. Optional `auto_pull_wait_seconds` may re-rank
and forward; `inflight_inc` only on that forward. Never forward a miss to a node
that lacks the model (Ollama would native-pull). Never `unsafe_single_node_mutate`.

## Streaming and retry

- Cap the request body before buffering.
- Retry another ranked node **only before the first upstream byte**.
- After the stream starts, pipe bytes; do not retry.
- `IncrementalCollector`: 1 MiB max unterminated NDJSON frame; do not mutate forwarded chunks.
- Nonretryable pre-response errors → Ollama-shaped **502**.

## Pull / delete

Persist jobs in SQLite (see durable-model-operations wiki). Recover via live
`/api/tags`. Do not log upstream bodies.

## Never

- Log prompts, bodies, embeddings, or tokens.
- Count health / `/api/ps` / admin / warm-keeper as client activity.
- Add Thunder or RunPod routes.
