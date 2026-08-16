## MODIFIED Requirements

### Requirement: Rank among holders by load, then size

The system SHALL forward `POST /api/generate`, `/api/chat`, `/api/embed` (including `/api/embeddings` rewritten to embed), and OpenAI `POST /v1/chat/completions`, `/v1/completions`, and `/v1/embeddings` only to healthy, non-draining nodes that already have the requested model on disk. Among those holders, it SHALL pick the lowest sort key: (1) inflight divided by base inflight cap, (2) RAM-pressure penalty, (3) known-GPU-util bias, (4) known-free-VRAM bias, (5) class VRAM preference, (6) warm versus cold. Utilization (inflight/cap) MUST dominate GPU-util bias, free-VRAM bias, and class preference. Known GPU-util gaps MUST dominate free-VRAM bias. Known free VRAM MUST rank a higher GiB ahead of a lower GiB. Unknown free VRAM MUST NOT sort as `0` (`size-load-routing`). Saturated holders MUST NOT be selected. If every otherwise-eligible holder is saturated, the client MUST receive 503 with reason `all_nodes_saturated`. If no healthy node has the model, the client MUST receive 503 with reason `model_missing` and MUST NOT be forwarded (native Ollama would Hub-pull). Tags, ps, show, version, admin, health, pull, and delete MUST NOT increment inflight or idle.

#### Scenario: two holders, different load

- **WHEN** two healthy GPUs both hold `qwen3:8b`, A is at higher inflight/cap than B, and other sort-key fields are equal
- **THEN** chat is forwarded to B

#### Scenario: miss is not a native pull

- **WHEN** no healthy node has `qwen3:8b` and `auto_pull_on_miss` is false
- **THEN** generate is not forwarded and the client gets 503 `model_missing` with no Retry-After

#### Scenario: saturated holders are not a fallback

- **WHEN** every healthy holder of the model is at its inflight cap
- **THEN** the client gets 503 `all_nodes_saturated` and no saturated node receives the request

#### Scenario: known GPU util breaks an inflight tie

- **WHEN** two healthy GPUs both hold `qwen3:8b`, inflight/cap and RAM pressure are equal, A has known GPU util 10% and B has known GPU util 80%
- **THEN** chat is forwarded to A

#### Scenario: inflight dominates GPU util

- **WHEN** two healthy GPUs both hold `qwen3:8b`, A is at 2/8 inflight with known GPU util 10%, and B is at 1/8 inflight with known GPU util 90%, and RAM pressure is equal
- **THEN** chat is forwarded to B

#### Scenario: known free VRAM breaks a GPU-util tie

- **WHEN** two healthy GPUs both hold `qwen3:8b`, inflight/cap, RAM pressure, and GPU-util bias are equal, A has known 8 GiB free and B has known 0.5 GiB free
- **THEN** chat is forwarded to A

#### Scenario: GPU util dominates free VRAM

- **WHEN** two healthy GPUs both hold `qwen3:8b`, inflight/cap and RAM pressure are equal, A has known GPU util 10% and 1 GiB free, and B has known GPU util 80% and 20 GiB free
- **THEN** chat is forwarded to A

#### Scenario: inflight dominates free VRAM

- **WHEN** two healthy GPUs both hold `qwen3:8b`, A is at 2/8 inflight with known 20 GiB free, and B is at 1/8 inflight with known 0.5 GiB free, and RAM pressure and GPU-util bias are equal
- **THEN** chat is forwarded to B

### Requirement: Class preference among similar load

When inflight utilization, RAM pressure, known-GPU-util bias, and known-free-VRAM bias do not already decide, the system SHALL apply class VRAM preference using **known** VRAM/GPU only: EMBED prefers lower known VRAM; SMALL prefers known GPU then unknown then known CPU; MEDIUM prefers lower known VRAM; LARGE prefers higher known VRAM. Omitted VRAM MUST NOT sort as `0` (lowest) for EMBED or MEDIUM. LARGE remains hard-gated by the LARGE VRAM estimate in `size-load-routing` (unknown VRAM is not in the LARGE pool).

#### Scenario: EMBED leaves the big card free

- **WHEN** an 8 GiB and a 48 GiB GPU both hold the embedding model, are healthy, and have equal inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias
- **THEN** embed ranks the 8 GiB node first

#### Scenario: EMBED does not treat unknown as the smallest GPU

- **WHEN** a known 8 GiB GPU and a holder with omitted VRAM both hold the same embedding model, are healthy, and have equal inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias
- **THEN** embed ranks the known 8 GiB GPU first

#### Scenario: SMALL prefers known GPU over unknown then CPU

- **WHEN** three healthy holders of the same SMALL model exist — known GPU, unknown VRAM/GPU count, and known CPU (`vram = 0`, `gpus = 0`) — with equal inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias
- **THEN** ranking order is known GPU, then unknown, then known CPU
