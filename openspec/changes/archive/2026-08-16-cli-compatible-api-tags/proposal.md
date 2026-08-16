## Why

`GET /api/tags` currently returns a name-only union (`name`, `model`, `details.router_nodes`). The official `ollama list` CLI slices `digest[:12]` with no length check, so pointing `OLLAMA_HOST` at the router panics. Native Ollama includes `digest`, `size`, `modified_at`, `details`, and `capabilities`. A transparent LAN proxy that clients treat as one Ollama must emit that list shape.

## What Changes

- Health `/api/tags` probes keep the list records needed for CLI display (not just model names).
- Aggregated `GET /api/tags` emits Ollama `ListModelResponse` fields: `name`, `model`, `digest` (≥12 hex chars when known), `size`, `modified_at`, native `details`, and `capabilities` when the probe supplied them.
- Rows still union by normalized model name across healthy, non-draining nodes. Extra `details.router_nodes` remains (unknown JSON fields are ignored by the CLI).
- When healthy nodes disagree on `digest` for the same name, pick one canonical row (newest `modified_at`, then stable node-id tie-break) rather than duplicating the name.
- `GET /v1/models` and `GET /v1/models/{id}` stay OpenAI-shaped (`id` / `object` / `created` / `owned_by`). Optionally set `created` from `modified_at` when present; no digest/size requirement on that surface.
- Tests cover CLI-required fields and a merge when two nodes advertise the same name with different digests.
- Docs/wiki: aggregated tags are a **CLI-compatible union**, not names-only.

## Capabilities

### New Capabilities

- `api-tags`: Aggregated `GET /api/tags` (and the OpenAI models list derived from the same union) MUST be a fleet union that remains compatible with the Ollama CLI `list` handler and native list JSON.

### Modified Capabilities

- (none — `openspec/specs/` has no existing capabilities)

## Impact

- `crates/ollama-router/src/health.rs` — parse and retain list records from `/api/tags` probes (names still drive `has_model` / routing).
- `crates/ollama-router-core/src/fleet/registry.rs` — store per-node tag records; `aggregated_tags` returns mergeable rows, not `(name, node_ids)` only.
- `crates/ollama-router/src/proxy/mod.rs` — serialize CLI-compatible JSON; keep `details.router_nodes`.
- `crates/ollama-router/tests/proxy.rs` — replace name-only assertions; add digest/size/`modified_at` and conflict-merge cases.
- Routing, streaming, pull/delete jobs, Verda, and idle timers are unchanged. Job recovery may keep using names only.
- Sensitivity: do not log tag JSON bodies; `digest` / `size` / `name` in the HTTP response are protocol fields, not secrets.
