## ADDED Requirements

### Requirement: Literal stop request unloads the whole fleet

A literal `POST /api/stop` request with a `model` SHALL follow the same fleet-unload contract as unload-intent generate/chat: the system MUST forward the unload to every healthy node that currently reports the model loaded (including operator-cordoned nodes) and MUST NOT forward to a single ranked holder. The response MUST be one CLI-compatible JSON object with `done: true` and `done_reason` `"unload"` once all targets have been attempted. A model loaded on no node MUST still succeed (idempotent). If any target fails, the client MUST receive HTTP 502 with an error object carrying a router-owned reason, and MUST NOT receive a success object; upstream error text MUST NOT be forwarded or logged. A missing `model` MUST be 400. The request MUST NOT set `last_client_request_at` and MUST NOT count as client inference inflight for idle teardown. The system MUST NOT log request bodies.

#### Scenario: stop unloads from every loaded node

- **WHEN** `qwen3:8b` is loaded on two healthy nodes and a client sends `POST /api/stop` with `{"model": "qwen3:8b"}`
- **THEN** both nodes receive an unload and the client gets one JSON object with `done: true` and `done_reason` `"unload"`

#### Scenario: stop of an unloaded model is success

- **WHEN** no node reports `gone:1b` loaded and a client sends `POST /api/stop` for it
- **THEN** the client gets a success object with `done_reason` `"unload"`

#### Scenario: stop with no model is a local 400

- **WHEN** a client sends `POST /api/stop` without a `model`
- **THEN** the client gets 400 and no upstream node receives a request

#### Scenario: stop does not reset the idle timer

- **WHEN** a client sends `POST /api/stop` while no inference is in flight
- **THEN** `last_client_request_at` is unchanged
