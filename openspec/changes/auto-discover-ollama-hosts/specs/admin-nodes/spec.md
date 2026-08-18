## ADDED Requirements

### Requirement: Discovered nodes never masquerade as cloud or fleet.yaml

A node created by host discovery, or by `POST /router/v1/nodes/enroll` with `origin: discovered` on an unknown id, SHALL be recorded with origin `discovered`, distinct from `permanent`, `adopt`, `verda`, and `runpod`. A discovered node MUST NOT count toward Verda or RunPod registered-scale caps, MUST NOT participate in orphan reclaim, and MUST NOT be eligible for cloud idle teardown. Discovered nodes SHALL appear with origin `discovered` in the `nodes` CLI, `/router/v1/readiness` counts, and the `node_info` metric. Discovery and this enroll path MUST NOT write `fleet.yaml`.

#### Scenario: Scan-created node is not Verda

- **WHEN** discovery adopts hostname `illuma` that is not in `fleet.yaml`
- **THEN** the node appears with origin `discovered`, cloud registered-instance counts are unchanged, and `node_info` reports `origin="discovered"`

#### Scenario: Enroll discovered does not write inventory

- **WHEN** an agent enrolls with `origin: discovered` and LAN URLs under a new id
- **THEN** the node is recorded as discovered, `fleet.yaml` is not written, and Verda orphan reclaim ignores it

### Requirement: LAN enroll does not require zrok share tokens

`POST /router/v1/nodes/enroll` SHALL accept `origin: discovered` (and MAY accept `origin: adopt`) with direct `ollama_url` and optional `capacity_url` instead of zrok share ids when those URLs are loopback, RFC1918, or Tailscale CGNAT (`100.64.0.0/10`). The request MUST still use the fail-closed admin bearer (unset token → 403, no default). Globally routable IPs and public-share hostnames MUST be rejected as `public_url_blocked`. Share-token enroll for `verda` / `runpod` / tunneled `fleet` MUST keep requiring private share ids. Enroll MUST NOT write `fleet.yaml`.

#### Scenario: LAN heartbeat enrolls without shares

- **WHEN** a node-agent POSTs `origin: discovered`, hostname-based id, `ollama_url: http://192.168.100.160:11435`, and `capacity_url: http://192.168.100.160:11436` with a valid admin bearer and no share ids
- **THEN** the node is reachable over those URLs, origin is `discovered`, and `fleet.yaml` is unchanged

#### Scenario: Public LAN enroll is refused

- **WHEN** enroll `origin: discovered` carries `ollama_url` on a globally routable IP or `*.zrok.io`
- **THEN** the response is `public_url_blocked` and no healthy node is created

#### Scenario: Unset admin token still fails closed

- **WHEN** `OLLAMA_ROUTER_ADMIN_TOKEN` is unset and a LAN enroll is posted
- **THEN** the response is 403 and no node is added

## MODIFIED Requirements

### Requirement: Readiness covers every inventory source

`GET /router/v1/readiness` SHALL report per-origin node counts covering `permanent`, `discovered`, `adopt`, `verda`, and `runpod`. The readiness recovery field SHALL reflect reconciling state from every enabled cloud manager: when RunPod is enabled, a RunPod pod that is still provisioning or reconciling SHALL be visible in readiness just as Verda instances are.

#### Scenario: RunPod reconciliation is visible in readiness

- **WHEN** RunPod is enabled and a pod is mid-provisioning (reconciling)
- **THEN** `/router/v1/readiness` counts the runpod node and the recovery field describes the RunPod reconciling state

#### Scenario: Adopt nodes are counted separately

- **WHEN** the fleet has permanent, adopt, Verda, and RunPod nodes
- **THEN** `/router/v1/readiness` counts each origin separately and no adopt node inflates the Verda count

#### Scenario: Discovered nodes are counted separately

- **WHEN** the fleet has permanent, discovered, adopt, Verda, and RunPod nodes
- **THEN** `/router/v1/readiness` includes a `discovered` count and that count does not inflate `permanent` or `verda`
