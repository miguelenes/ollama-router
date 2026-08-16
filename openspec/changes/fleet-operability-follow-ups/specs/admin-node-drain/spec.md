## Purpose

Lets an operator cordon a node for maintenance through the admin API: a draining node stops receiving new work but keeps health probes and is never destroyed by the drain itself.

## ADDED Requirements

### Requirement: Operator drain and undrain via the admin API

The system SHALL expose `POST /router/v1/nodes/{id}/drain` and `POST /router/v1/nodes/{id}/undrain` behind the fail-closed admin bearer (unset token MUST be 403 with no default secret). Draining a node MUST exclude it from inference ranking and from new placement, bootstrap, and warm-keeper targets, while health and capacity probes continue. Drain MUST NOT destroy the node, MUST NOT cancel its in-flight requests, and MUST NOT write `fleet.yaml`. Draining a `fleet.yaml` host MUST NOT make it destroyable. Undrain MUST restore eligibility. Both operations MUST be idempotent; an unknown node id MUST be 404. The draining state MUST be visible in the admin nodes listing and the `ollama_router_node_draining` gauge.

#### Scenario: drained node receives no new work

- **WHEN** an operator drains a healthy holder of `qwen3:8b` and a client sends chat for that model
- **THEN** the drained node is not selected; if it was the only holder the client gets the existing 503

#### Scenario: drain is not destroy

- **WHEN** an operator drains a `fleet.yaml` host
- **THEN** the host keeps its health probes, is never destroyed, and `fleet.yaml` is unchanged

#### Scenario: undrain restores eligibility

- **WHEN** an operator undrains a previously drained healthy holder
- **THEN** the node is rankable again for new requests

#### Scenario: drain is fail-closed

- **WHEN** `OLLAMA_ROUTER_ADMIN_TOKEN` is unset and a drain request arrives
- **THEN** the response is 403 and no node state changes
