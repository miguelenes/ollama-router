# api-pull Specification

## Purpose
Streams `POST /api/pull` as NDJSON progress for a fleet placement job so the official CLI can watch status and totals without treating the router as a single Hub-pull daemon.

## Requirements

### Requirement: HTTP pull streams placement-job NDJSON

The system SHALL handle `POST /api/pull` as a placement job that targets healthy generate-class-eligible nodes (`model-placement`), not as a native Hub-pull through one ranked node. The response MUST be an NDJSON stream (`Content-Type` `application/x-ndjson`) of status objects the official Ollama CLI can consume (`status`, and `total` / `completed` when a progress denominator is known). The system MUST flush lines as job targets advance and MUST NOT buffer the full job into one body. A fully successful job (including all targets already present or skipped for capacity/disk/unhealthy) MUST end with a line whose `status` is `success`. A missing `model` MUST be 400 without a stream. Zero placement-eligible targets MUST be 503 with reason `insufficient_capacity` (existing Retry-After policy) and MUST NOT emit a `success` line. Partial target failure MUST NOT emit `success`; the client MUST see an `error` object with a router reason (not upstream provider text). Client disconnect MUST end the HTTP stream and MUST NOT cancel the job. The system MUST NOT log pull bodies, prompts, or per-target `detail` strings. SQLite MUST store operation metadata only.

#### Scenario: CLI can consume a successful pull stream

- **WHEN** a client pulls `llama3.2:3b` and every placement-eligible target succeeds or is already present
- **THEN** the response is NDJSON with `Content-Type` `application/x-ndjson` and a final object `{"status":"success"}`

#### Scenario: pull is not a single-node native mutate

- **WHEN** two healthy GPUs are generate-class-eligible for `qwen3:8b` and the client posts `/api/pull`
- **THEN** both GPUs are job targets; the stream is not a passthrough of one node’s Hub-pull body

#### Scenario: no eligible targets is 503 not success

- **WHEN** the only nodes are a known CPU and an unknown-VRAM node and the client pulls `llama3.1:70b`
- **THEN** the client gets 503 `insufficient_capacity` and no NDJSON `success` line

#### Scenario: client drop does not cancel the job

- **WHEN** a client disconnects during `POST /api/pull` after the placement job has started
- **THEN** the HTTP stream ends and the job continues (it is not cancelled)

### Requirement: Pull stays operationally quiet for idle

`POST /api/pull` MUST NOT increment `last_client_request_at`. Job work MUST NOT be treated as client inference inflight for idle teardown.

#### Scenario: pull does not reset the idle timer

- **WHEN** a client calls `POST /api/pull` while no generate/chat/embed is in flight
- **THEN** `last_client_request_at` is unchanged
