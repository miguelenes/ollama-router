## Purpose

Exposes a fleet-wide view of currently loaded models through `GET /api/ps` so `ollama ps` against the router URL shows every healthy node’s resident set, not one ranked daemon.

## ADDED Requirements

### Requirement: Aggregated ps is a CLI-compatible loaded union

The system SHALL serve `GET /api/ps` as a JSON object with a `models` array that is the union of models currently loaded on healthy, non-draining nodes (from those nodes’ last successful `/api/ps` probes). The response MUST NOT be a passthrough to a single ranked node. Each element MUST include `name` and `model`. When the originating probe supplied `size`, `size_vram`, `digest`, native `details`, `expires_at`, or `context_length`, that row MUST include those fields. When the system emits `digest`, it MUST be a string of at least 12 characters. Each row MUST identify the node that reported it (`details.router_node`). Unhealthy or draining nodes MUST NOT contribute rows.

#### Scenario: two nodes have the same model loaded

- **WHEN** healthy nodes `a` and `b` both report `qwen3:8b` loaded on their `/api/ps` probes
- **THEN** `GET /api/ps` contains two `qwen3:8b` objects, one with `details.router_node` `a` and one with `b`

#### Scenario: unhealthy nodes are omitted

- **WHEN** only one of two nodes that have `llama3.2:1b` loaded is healthy
- **THEN** `GET /api/ps` includes that loaded model only for the healthy node

#### Scenario: empty loaded set

- **WHEN** every healthy node’s last ps probe reported no loaded models
- **THEN** `GET /api/ps` returns `{"models":[]}` and does not forward to an upstream `/api/ps`

### Requirement: Ps listing stays operationally quiet

Client `GET /api/ps` and health `/api/ps` probes MUST NOT count as client inflight or idle activity. The system MUST NOT log ps response bodies, prompts, or embeddings. Model `name`, `digest`, `size`, `size_vram`, and node ids MAY appear in the HTTP response.

#### Scenario: listing loaded models does not mark the fleet busy

- **WHEN** a client calls `GET /api/ps` while no generate/chat/embed is in flight
- **THEN** node inflight counters and `last_client_request_at` are unchanged
