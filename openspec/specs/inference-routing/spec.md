# inference-routing Specification

## Purpose
Forwards generate, chat, and embed across a cluster as one URL: only nodes that already have the model, chosen by load first and size-class preference second, without buffering the stream.

## Requirements

### Requirement: Rank among holders by load, then size

The system SHALL forward `POST /api/generate`, `/api/chat`, `/api/embed` (including `/api/embeddings` rewritten to embed), and OpenAI `POST /v1/chat/completions`, `/v1/completions`, and `/v1/embeddings` only to healthy, non-draining nodes that already have the requested model on disk. Among those holders, it SHALL pick the lowest sort key: (1) inflight divided by base inflight cap, (2) RAM-pressure penalty, (3) class VRAM preference, (4) warm versus cold. Utilization MUST dominate class preference. Saturated holders MUST NOT be selected. If every otherwise-eligible holder is saturated, the client MUST receive 503 with reason `all_nodes_saturated`. If no healthy node has the model, the client MUST receive 503 with reason `model_missing` and MUST NOT be forwarded (native Ollama would Hub-pull). Tags, ps, show, admin, and health MUST NOT increment inflight or idle.

#### Scenario: two holders, different load

- **WHEN** two healthy GPUs both hold `qwen3:8b`, A is at higher inflight/cap than B, and other sort-key fields are equal
- **THEN** chat is forwarded to B

#### Scenario: miss is not a native pull

- **WHEN** no healthy node has `qwen3:8b` and `auto_pull_on_miss` is false
- **THEN** generate is not forwarded and the client gets 503 `model_missing` with no Retry-After

#### Scenario: saturated holders are not a fallback

- **WHEN** every healthy holder of the model is at its inflight cap
- **THEN** the client gets 503 `all_nodes_saturated` and no saturated node receives the request

### Requirement: Class preference among similar load

When utilization (and pressure) do not already decide, the system SHALL apply class VRAM preference using **known** VRAM/GPU only: EMBED prefers lower known VRAM; SMALL prefers known GPU then unknown then known CPU; MEDIUM prefers lower known VRAM; LARGE prefers higher known VRAM. Omitted VRAM MUST NOT sort as `0` (lowest) for EMBED or MEDIUM. LARGE remains hard-gated by the LARGE VRAM estimate in `size-load-routing` (unknown VRAM is not in the LARGE pool).

#### Scenario: EMBED leaves the big card free

- **WHEN** an 8 GiB and a 48 GiB GPU both hold the embedding model, are healthy, and have equal utilization
- **THEN** embed ranks the 8 GiB node first

#### Scenario: EMBED does not treat unknown as the smallest GPU

- **WHEN** a known 8 GiB GPU and a holder with omitted VRAM both hold the same embedding model, are healthy, and have equal utilization
- **THEN** embed ranks the known 8 GiB GPU first

#### Scenario: SMALL prefers known GPU over unknown then CPU

- **WHEN** three healthy holders of the same SMALL model exist — known GPU, unknown VRAM/GPU count, and known CPU (`vram = 0`, `gpus = 0`) — with equal utilization
- **THEN** ranking order is known GPU, then unknown, then known CPU

### Requirement: Stream and retry only before the first byte

The system SHALL stream NDJSON or SSE chunks as they arrive and MUST NOT buffer the full inference body. It MAY retry another ranked holder only before the first upstream response byte. After the stream has begun, it MUST NOT retry.

#### Scenario: retry stops after the stream starts

- **WHEN** the chosen holder has already sent the first body byte
- **THEN** a later upstream error is not retried on another node
