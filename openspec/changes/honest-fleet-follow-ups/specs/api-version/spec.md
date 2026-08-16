## Purpose

Serves `GET /api/version` as the router process version so clients probing the listen URL do not receive a random node's Ollama build.

## ADDED Requirements

### Requirement: Version is router-owned, not a passthrough

The system SHALL serve `GET /api/version` as JSON `{"version": "<router version>"}` from the router process (same version string as `GET /healthz`). The response MUST NOT be forwarded to a ranked node's `/api/version`. The system MUST NOT log the body.

#### Scenario: version does not hit a node

- **WHEN** a healthy node would answer `GET /api/version` with a different Ollama build
- **THEN** the client's `GET /api/version` against the router returns the router version and that node is not contacted

### Requirement: Version stays operationally quiet

`GET /api/version` MUST NOT increment inflight or `last_client_request_at`.

#### Scenario: version does not mark the fleet busy

- **WHEN** a client calls `GET /api/version` while no generate/chat/embed is in flight
- **THEN** node inflight counters and `last_client_request_at` are unchanged
