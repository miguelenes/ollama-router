# api-delete Specification

## Purpose
Streams `DELETE /api/delete` as NDJSON progress for a fleet delete job so `ollama rm` against the router can watch status without treating the router as a single daemon.

## Requirements

### Requirement: HTTP delete streams fleet-job NDJSON

The system SHALL handle `DELETE /api/delete` as a fleet delete job (every **healthy, non-draining** node that currently holds the model), not as a native delete through one ranked node. The response MUST be an NDJSON stream (`Content-Type` `application/x-ndjson`) of status objects the official Ollama CLI can consume (`status`, and `total` / `completed` when a progress denominator is known). The system MUST flush lines as job targets advance and MUST NOT buffer the full job into one body. A fully successful job (including already absent, skipped, or no remaining holders) MUST end with a line whose `status` is `success`. A missing `model` MUST be 400 without a stream. Partial target failure MUST NOT emit `success`; the client MUST see an `error` object with a router reason (not upstream provider text). Client disconnect MUST end the HTTP stream and MUST NOT cancel the job. The system MUST NOT log delete bodies, prompts, or per-target `detail` strings. SQLite MUST store operation metadata only.

#### Scenario: CLI can consume a successful delete stream

- **WHEN** a client deletes `llama3.2:3b` and every holder succeeds or the model is already absent
- **THEN** the response is NDJSON with `Content-Type` `application/x-ndjson` and a final object `{"status":"success"}`

#### Scenario: delete is not a single-node native mutate

- **WHEN** two healthy, non-draining nodes hold `qwen3:8b` and the client sends `DELETE /api/delete`
- **THEN** both nodes are job targets; the stream is not a passthrough of one node’s native delete body

#### Scenario: already absent is success

- **WHEN** no node has `gone:1b` on disk
- **THEN** the client gets a success NDJSON stream (no error); the model is not required to exist

#### Scenario: client drop does not cancel the job

- **WHEN** a client disconnects during `DELETE /api/delete` after the delete job has started
- **THEN** the HTTP stream ends and the job continues (it is not cancelled)

### Requirement: Delete stays operationally quiet for idle

`DELETE /api/delete` MUST NOT increment `last_client_request_at`. Job work MUST NOT be treated as client inference inflight for idle teardown.

#### Scenario: delete does not reset the idle timer

- **WHEN** a client calls `DELETE /api/delete` while no generate/chat/embed is in flight
- **THEN** `last_client_request_at` is unchanged
