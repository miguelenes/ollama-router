---
name: ollama-compat-proxy
description: Implements the Ollama-compatible proxy surface for this fleet router — generate/chat/embed, /api/embeddings rewrite, aggregated /api/tags, fleet pull/delete, and 503+Retry-After on capacity miss. Use when adding or changing HTTP routes, proxy streaming, admin ensure/delete, or Ollama protocol compatibility.
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
| `POST /api/pull`, `/api/delete` | Always fleet orchestrator (stub → 503 until jobs land). Prefer admin. |
| `GET /healthz` | Process up. |
| `GET /readyz` | Healthy capacity (optional embedding-model gate). |
| `GET /metrics` | Prometheus. |
| `/router/v1/*` | Admin bearer. Unset token → 403. |

## Capacity miss

Map `no_nodes` / `no_healthy` / `model_missing` / `capacity` / `ram` /
`ram_pressure` / `saturated` to Ollama-shaped **503** JSON and set
`Retry-After` (30s). Kick coalesced async Verda `ensure`. Never block the
client on provision.

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
