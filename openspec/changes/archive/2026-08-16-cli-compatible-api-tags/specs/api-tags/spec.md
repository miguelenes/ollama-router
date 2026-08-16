## Purpose

Exposes a fleet-wide model catalog through `GET /api/tags` and the OpenAI models list so clients, including the official Ollama CLI `list` command, can treat the router as one Ollama.

## ADDED Requirements

### Requirement: Aggregated tags are a CLI-compatible union

The system SHALL serve `GET /api/tags` as a JSON object with a `models` array that is the union of on-disk models reported by healthy, non-draining nodes. Each element MUST include `name`, `model`, and `digest`. `digest` MUST be a string of at least 12 characters so the Ollama CLI `list` handler can slice `digest[:12]` without panicking. When a node's tags probe supplied `size`, `modified_at`, native `details`, or `capabilities`, the aggregated row MUST include those fields. The response MAY add `details.router_nodes` listing node ids that hold the model. Unhealthy or draining nodes MUST NOT contribute rows.

#### Scenario: ollama list against a one-node fleet

- **WHEN** a healthy node has reported a tags probe with `name`, a 64-character `digest`, `size`, and `modified_at`
- **THEN** `GET /api/tags` returns that model with the same `digest`, `size`, and `modified_at` (not a name-only object)

#### Scenario: empty digest is never returned

- **WHEN** a healthy node has a model name but no upstream digest (or a digest shorter than 12 characters)
- **THEN** `GET /api/tags` still returns that model with a `digest` of at least 12 characters

#### Scenario: unhealthy nodes are omitted

- **WHEN** only one of two nodes that hold `llama3.2:1b` is healthy
- **THEN** the aggregated row for `llama3.2:1b` includes that healthy node in `details.router_nodes` and does not list the unhealthy node

### Requirement: One row per normalized model name

The system SHALL emit at most one `models[]` entry per normalized model name. When two healthy nodes advertise the same name with different `digest` values, the system MUST keep a single canonical row: prefer the record with the newest `modified_at`; if `modified_at` is missing or tied, prefer the lexicographically smaller node id. `details.router_nodes` MUST still list every healthy holder.

#### Scenario: same name, different digests

- **WHEN** node `a` reports `llama3.2:1b` with digest `aaa…` and an older `modified_at`, and node `b` reports `llama3.2:1b` with digest `bbb…` and a newer `modified_at`
- **THEN** `GET /api/tags` contains exactly one object named `llama3.2:1b` whose `digest` is `bbb…` and whose `details.router_nodes` includes both `a` and `b`

#### Scenario: same name, same digest

- **WHEN** two healthy nodes report the same name and the same digest
- **THEN** `GET /api/tags` contains exactly one object for that name with that digest and both node ids in `details.router_nodes`

### Requirement: OpenAI models list stays a name union

The system SHALL serve `GET /v1/models` and `GET /v1/models/{id}` from the same healthy-node union as `/api/tags`, in OpenAI list/retrieve shape (`id`, `object`, `created`, `owned_by`). Those responses MUST NOT require `digest` or `size`. When `modified_at` is known for a model, `created` MUST be that timestamp as Unix seconds; otherwise `created` MUST be `0`. A missing id MUST return 404 with the OpenAI error envelope.

#### Scenario: list models in OpenAI shape

- **WHEN** the fleet union includes `qwen3-embedding:8b`
- **THEN** `GET /v1/models` includes an entry with `id` `qwen3-embedding:8b`, `object` `model`, and `owned_by` `library`

#### Scenario: retrieve unknown model

- **WHEN** a client requests `GET /v1/models/does-not-exist`
- **THEN** the response is 404 with an OpenAI-shaped error body and code `model_not_found`

### Requirement: Tags probes stay operationally quiet

Tags probes and aggregated catalog responses MUST NOT count as client inflight or idle activity. The system MUST NOT log tag response bodies, prompts, or embeddings. Model `name`, `digest`, `size`, and node ids MAY appear in the HTTP response.

#### Scenario: listing models does not mark the fleet busy

- **WHEN** a client calls `GET /api/tags` while no generate/chat/embed is in flight
- **THEN** node inflight counters and `last_client_request_at` are unchanged
