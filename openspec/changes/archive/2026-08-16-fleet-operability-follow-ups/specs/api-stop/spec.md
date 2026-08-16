## Purpose

Makes `ollama stop` honest against the fleet: an unload-intent generate/chat (`keep_alive <= 0`, no prompt/messages) unloads the model from every loaded holder, not just one ranked node.

## ADDED Requirements

### Requirement: Stop unloads every loaded holder

The system SHALL treat `POST /api/generate` with `keep_alive <= 0` and an empty or absent `prompt`, and `POST /api/chat` with `keep_alive <= 0` and an empty or absent `messages` array, as a fleet **unload**: it MUST forward the unload to every healthy node that currently reports the model loaded, including operator-cordoned nodes, and MUST NOT target nodes that are draining for inventory-remove or Verda teardown. It MUST NOT forward to a single ranked holder. The response MUST be one CLI-compatible JSON object with `done: true` and `done_reason` `"unload"` once all targets have been attempted. A model loaded on no node MUST still succeed (idempotent). If any target fails, the client MUST receive HTTP 502 with an error object with a router-owned reason and MUST NOT receive a success object; upstream error text MUST NOT be forwarded or logged. A missing `model` MUST be 400. A generate/chat with a non-empty prompt/messages or `keep_alive > 0` MUST remain normal ranked inference. The system MUST NOT log request bodies.

#### Scenario: stop unloads from every loaded node

- **WHEN** `qwen3:8b` is loaded on two healthy nodes and a client sends `POST /api/generate` with `{"model": "qwen3:8b", "keep_alive": 0}`
- **THEN** both nodes receive an unload and the client gets one JSON object with `done: true` and `done_reason` `"unload"`

#### Scenario: stop of an unloaded model is success

- **WHEN** no node reports `gone:1b` loaded and a client sends the unload-intent generate
- **THEN** the client gets a success object (unload is idempotent)

#### Scenario: stop still unloads a cordoned holder

- **WHEN** an operator has drained a healthy holder that still reports the model loaded and a client sends an unload-intent generate
- **THEN** that holder receives the unload (cordon does not hide loaded models from stop)

#### Scenario: normal generate is not hijacked

- **WHEN** a client sends `POST /api/generate` with a non-empty `prompt` and `keep_alive: 0`
- **THEN** the request is ranked and forwarded as normal inference, not fanned out as an unload

### Requirement: Stop stays operationally quiet for idle

An unload-intent generate/chat MUST NOT set `last_client_request_at` and MUST NOT count as client inference inflight for idle teardown.

#### Scenario: stop does not reset the idle timer

- **WHEN** a client sends an unload-intent generate while no inference is in flight
- **THEN** `last_client_request_at` is unchanged
