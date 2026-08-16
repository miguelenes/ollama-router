## Purpose

Forwards `POST /api/show` only to a node that already has the named model on disk so `ollama show` against the router inspects a real holder, not an arbitrary ranked daemon.

## ADDED Requirements

### Requirement: Show is forwarded only to a healthy holder

The system SHALL forward `POST /api/show` only to a healthy, non-draining node that already has the requested model on disk (`model` or `name` in the body). Among those holders it SHALL use the same load-then-size ranking as other non-inference holder picks (GENERIC class; LARGE VRAM gates MUST NOT exclude a holder). If no healthy node has the model, the client MUST receive 503 with reason `model_missing` and MUST NOT be forwarded. The system MUST stream the upstream body as it arrives and MUST NOT log show bodies, modelfiles, prompts, or embeddings.

#### Scenario: show hits a holder, not a non-holder

- **WHEN** node `gpu` holds `llama3.1:70b` on disk and node `cpu` does not, and both are healthy
- **THEN** `POST /api/show` for that name is forwarded to `gpu` and is not forwarded to `cpu`

#### Scenario: show miss is not a native 404 from the wrong node

- **WHEN** no healthy node has `missing:7b` on disk
- **THEN** show is not forwarded and the client gets 503 `model_missing`

#### Scenario: show is not LARGE-gated

- **WHEN** the only healthy holder of `llama3.1:70b` has unknown VRAM
- **THEN** show is still forwarded to that holder (GENERIC; not 503 `insufficient_capacity`)

### Requirement: Show stays operationally quiet

`POST /api/show` MUST NOT increment inflight or `last_client_request_at`. Classification remains GENERIC (`request-class`).

#### Scenario: show does not mark the fleet busy

- **WHEN** a client calls `POST /api/show` for a model a healthy node holds
- **THEN** that node’s inflight counter and `last_client_request_at` are unchanged
