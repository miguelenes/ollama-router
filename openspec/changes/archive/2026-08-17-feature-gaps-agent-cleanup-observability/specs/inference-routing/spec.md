## ADDED Requirements

### Requirement: Wrong-method and unknown requests never leak upstream

The system SHALL answer any request whose method and path do not match a supported operation with a deterministic router-owned response and MUST NOT forward it to a ranked node. Paths that are never fleet operations — `/api/create`, `/api/copy`, `/api/push`, and `/api/blobs/*` — SHALL keep their 501 for every method and MUST NOT be re-routed to 405. A known path reached with an unsupported method (for example `POST /api/tags`, `GET /api/generate`, `GET /v1/chat/completions`) SHALL return 405. An unknown path under a supported prefix (`/api/*`, `/v1/*`) SHALL return 404. The error body SHALL follow the request path: the Ollama error object on `/api/*`, the OpenAI error envelope on `/v1/*`. These responses MUST NOT read request bodies beyond the existing size cap, MUST NOT increment inflight or the idle timer, and MUST NOT contact any upstream node.

#### Scenario: Wrong method on a known Ollama path is a local 405

- **WHEN** a client sends `POST /api/tags` (a known path with the wrong method)
- **THEN** the client gets 405 with an Ollama error object, no upstream node receives a request, and no inflight or idle counter moves

#### Scenario: Wrong method on a known OpenAI path is a local 405

- **WHEN** a client sends `GET /v1/chat/completions`
- **THEN** the client gets 405 with an OpenAI error envelope and no upstream node is contacted

#### Scenario: Unknown Ollama path is a local 404

- **WHEN** a client sends `GET /api/not-a-real-endpoint`
- **THEN** the client gets 404 with an Ollama error object and the request is not forwarded to a ranked node

### Requirement: Known OpenAI mutation paths are rejected as non-fleet operations

`DELETE /v1/models/{id}` and any `POST /v1/fine_tuning/*` request SHALL return 501 with an OpenAI error envelope, matching the honest-fleet treatment of native mutate endpoints (`/api/create`, `/api/copy`, `/api/push`, `/api/blobs`), instead of a 404 that is indistinguishable from a typo. The response MUST NOT contact upstream.

#### Scenario: OpenAI model delete is an explicit 501

- **WHEN** a client sends `DELETE /v1/models/qwen3:8b`
- **THEN** the client gets 501 with an OpenAI error envelope and no upstream node receives a request

#### Scenario: OpenAI fine-tuning is an explicit 501

- **WHEN** a client sends `POST /v1/fine_tuning/jobs`
- **THEN** the client gets 501 with an OpenAI error envelope and no upstream node receives a request
