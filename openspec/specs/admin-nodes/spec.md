# admin-nodes Specification

## Purpose
Administers node registration and readiness reporting: adopt nodes keep their own origin instead of masquerading as cloud instances, and readiness counts and recovery cover every inventory source including both cloud providers.

## Requirements

### Requirement: Adopt nodes never masquerade as cloud instances

A node created through the debug/adopt admin surface — `PUT /router/v1/nodes` with a new node id, or `POST /router/v1/nodes/enroll` with `origin: adopt` on an unknown id — SHALL be recorded with an `adopt` origin distinct from Verda and RunPod. An adopt node MUST NOT count toward Verda registered-scale caps, MUST NOT participate in Verda orphan reclaim, and MUST NOT be eligible for Verda idle teardown. Adopt nodes SHALL be reported with origin `adopt` in the `nodes` CLI output, the `/router/v1/readiness` counts, and the `node_info` metric. Adopt enrollment MUST NOT write `fleet.yaml`.

#### Scenario: Admin-created node is not a Verda instance

- **WHEN** an operator runs `PUT /router/v1/nodes` with a brand-new id and a private-share URL
- **THEN** the node appears with origin `adopt`, the Verda manager's registered-instance count is unchanged, and the `node_info` metric reports `origin="adopt"`

#### Scenario: Enroll adopt does not collide with Verda orphan reclaim

- **WHEN** a node enrolls with `origin: adopt` under an id that resembles a Verda instance id
- **THEN** the node is recorded as adopt, Verda orphan reclaim ignores it, and `fleet.yaml` is not written

### Requirement: Readiness covers every inventory source

`GET /router/v1/readiness` SHALL report per-origin node counts covering `permanent`, `adopt`, `verda`, and `runpod`. The readiness recovery field SHALL reflect reconciling state from every enabled cloud manager: when RunPod is enabled, a RunPod pod that is still provisioning or reconciling SHALL be visible in readiness just as Verda instances are.

#### Scenario: RunPod reconciliation is visible in readiness

- **WHEN** RunPod is enabled and a pod is mid-provisioning (reconciling)
- **THEN** `/router/v1/readiness` counts the runpod node and the recovery field describes the RunPod reconciling state

#### Scenario: Adopt nodes are counted separately

- **WHEN** the fleet has permanent, adopt, Verda, and RunPod nodes
- **THEN** `/router/v1/readiness` counts each origin separately and no adopt node inflates the Verda count
