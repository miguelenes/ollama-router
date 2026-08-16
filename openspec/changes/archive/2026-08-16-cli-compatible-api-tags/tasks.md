## 1. Registry catalog records

- [x] 1.1 Add a `TagRecord` (digest, size, modified_at, details JSON, capabilities) on each node's cold state, keyed by normalized name, without changing `has_model` / the name `HashSet`
- [x] 1.2 On tags update, replace both the name set and the record map; drop records for names the probe no longer lists
- [x] 1.3 Change `aggregated_tags` to return one merged row per name (winning record + sorted `router_nodes`), using newest parseable `modified_at` then smaller node id; skip unhealthy/draining nodes
- [x] 1.4 When the winning digest is missing or shorter than 12 characters, fill a stable SHA-256 hex of the normalized name (≥12 chars). Cover merge, placeholder, and unhealthy omission with unit tests next to the registry module

## 2. Health probe

- [x] 2.1 Parse `/api/tags` models with `name`, `model`, `digest`, `size`, `modified_at`, `details`, and `capabilities`; ignore unknown fields; do not log the body
- [x] 2.2 Write parsed records into the registry on a successful probe (names still drive health/routing)

## 3. Proxy JSON

- [x] 3.1 Serialize `GET /api/tags` with `name`, `model`, `digest`, plus probe `size` / `modified_at` / native `details` / `capabilities`, merging `details.router_nodes`
- [x] 3.2 Set OpenAI `GET /v1/models` and `GET /v1/models/{id}` `created` from the winning `modified_at` Unix seconds when parseable, else `0`; keep 404 `model_not_found`
- [x] 3.3 Extend proxy tests: CLI fields from injected records, name-only `mark_ready` still yields `digest` len ≥ 12, two-node digest conflict, unhealthy omission, OpenAI `created`, and tags not incrementing inflight

## 4. Docs and gate

- [x] 4.1 Update the product wiki and `ollama-compat-proxy` skill: aggregated tags are a CLI-compatible union (not names-only); note placeholder digests
- [x] 4.2 Run `task check`. Against a running `task dev` stack, confirm `OLLAMA_HOST=http://127.0.0.1:11435 ollama list` prints NAME/ID/SIZE/MODIFIED without panicking
