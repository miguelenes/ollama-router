---
name: ollama-compat-proxy
description: Implements the Ollama-compatible proxy surface for this fleet router — generate/chat/embed, OpenAI /v1 chat/completions/embeddings passthrough, /api/embeddings rewrite, aggregated /api/tags and /v1/models, fleet pull/delete, and 503+Retry-After on capacity miss. Use when adding or changing HTTP routes, proxy streaming, admin ensure/delete, or Ollama protocol compatibility.
---

# Ollama-compatible proxy

Read `.opencode/wiki/concepts/ollama-router-product.md` and
`.opencode/wiki/concepts/ollama-router-phase-3-retry-and-memory-safety.md`.
Code lives under `crates/ollama-router/src/proxy/` and `.../http/`.

## Surface

Honest fleet contract: **list** = union of holders; **infer** = holders-only WLC;
**pull** = placement job on healthy generate-class-eligible nodes (not a native
NDJSON stub through one daemon); **miss** = 503 `model_missing` (default, no
auto Hub-pull); create/copy/push/blobs = 501.

| Path | Behavior |
|------|----------|
| `POST /api/generate`, `/api/chat` | Stream NDJSON. `inflight_inc` (idle activity). Size class may use catalog `details.parameter_size` when `:Nb` is absent. |
| `POST /api/embed` | Stream/JSON. `inflight_inc`. |
| `POST /api/embeddings` | Rewrite path to `/api/embed` (Ollama ≤0.32). Then same as embed. |
| `POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings` | Passthrough to Ollama's OpenAI shim on the ranked node. Same `inflight_inc` / reservation / class ranking as native chat/embed. Do **not** rewrite `/v1/embeddings` to `/api/embed`. |
| `GET /api/tags` | CLI-compatible **union** of healthy nodes (not names-only). Each row has `name`, `model`, `digest` (≥12 chars; SHA-256 hex of the normalized name when the probe omitted digest), plus probe `size` / `modified_at` / native `details` / `capabilities`. `details.router_nodes` lists holders. Not a passthrough. Not idle. |
| `GET /v1/models` | Same union in OpenAI list format (`id` / `object` / `created` from `modified_at` Unix seconds else `0` / `owned_by: library`). |
| `GET /v1/models/{id}` | Retrieve from that union (404 OpenAI-shaped if absent). Not a client forward. |
| `POST /api/show` | Holder-only (`model` or `name`). GENERIC class (not LARGE-gated). Miss → 503 `model_missing`. Stream upstream body. Not idle. |
| `GET /api/ps` | Process-list **union** of healthy loaded models (one row per node × model, `details.router_node`, digest ≥ 12). Not a passthrough. Not idle. |
| `GET /api/version` | Router-owned `{"version": "<router>"}` (same as `/healthz`). Not a ranked Ollama. Not idle. |
| `POST /api/push`, `/api/copy`, `/api/create`, `/api/blobs*` | **501** `not_a_fleet_operation`. Use admin ensure / `POST /api/pull`. |
| Other `/v1/*` | **404** OpenAI-shaped. Allowlist only. |
| `POST /api/pull` | Fleet placement job; streams NDJSON (`application/x-ndjson`) with `total`/`completed` from targets and final `success`. Not a one-node Hub-pull. Not idle. |
| `DELETE /api/delete` | Fleet delete job on healthy non-draining holders; streams NDJSON like pull (`total`/`completed`, final `success`). Already-absent is success. Not idle. Prefer admin for JSON. |
| `GET /healthz` | Process up. |
| `GET /readyz` | Healthy capacity (optional embedding-model gate). |
| `GET /metrics` | Prometheus. Count-only model gauges (`aggregated_models`, `node_models`) plus `discovery_total`. Never a model-name label. Grafana Models row joins agent `ollama_up` / `ollama_models`. |
| `/router/v1/*` | Admin bearer. Unset token → 403. |

## Capacity miss

Map `no_nodes` / `no_healthy` / `capacity` / `ram` / `ram_pressure` /
`saturated` to **503** JSON and set `Retry-After`. Body shape follows the
request path: Ollama `{"error": "…"}` on `/api/*`, OpenAI
`{"error": {"message", "type", "code"}}` on `/v1/*` (not `/router/v1/*`).
Kick coalesced async Verda demand-scale (`create_additional`) for those
reasons. Never block the client on provision.

Omitted node VRAM is **unknown** (not a CPU). LARGE/MEDIUM against unknown-only
holders → `insufficient_capacity`. SMALL/EMBED may still forward to unknown.

`model_missing` has **no** Retry-After when `auto_pull_on_miss` is false (default).
When the flag is on, the proxy enqueues `start_ensure(Placement)` on
`placement_eligible_node_ids` only (static VRAM/class — LARGE never lands on
CPU or unknown VRAM). Empty placement → `insufficient_capacity` + provision Retry-After + demand
scale. Else 503 JSON `reason=pull_enqueued` + `Retry-After` from
`pull_miss_retry_after_seconds`. Optional `auto_pull_wait_seconds` may re-rank
and forward; `inflight_inc` only on that forward. Never forward a miss to a node
that lacks the model (Ollama would native-pull). Never `unsafe_single_node_mutate`.

## Streaming and retry

- Cap the request body before buffering.
- Retry another ranked node **only before the first upstream byte**.
- After the stream starts, pipe bytes; do not retry.
- `IncrementalCollector`: 1 MiB max unterminated frame; NDJSON or SSE (`text/event-stream`); do not mutate forwarded chunks.
- Nonretryable pre-response errors → **502**, shape follows the request path.

## Pull / delete

Persist jobs in SQLite (see durable-model-operations wiki). Recover via live
`/api/tags`. Default pull places on every healthy generate-class-eligible node.
HTTP pull and delete stream fleet-job NDJSON; known insufficient disk skips a
pull target (`skipped_disk`); unknown disk does not. Delete targets healthy
non-draining holders; already-absent is a success stream. Opt-in
`bootstrap_desired_models` background-ensures desired tiers (known VRAM ∩
`min_vram_gb`). Do not log upstream bodies. Pull is not a stub NDJSON Hub-pull
through one node.

## Never

- Log prompts, bodies, embeddings, or tokens.
- Count health / `/api/tags` / `/v1/models` / `/api/ps` / admin / warm-keeper as client activity.
- Add Thunder or RunPod routes.
- Treat omitted VRAM as a measured CPU for ranking or placement.
