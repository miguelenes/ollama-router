## ADDED Requirements

### Requirement: Swarm example enables discovery without listing backend IPs

The swarm deploy example tunables SHALL show `discovery.enabled` with the home LAN CIDR (`192.168.100.0/24`) and Tailscale peer enumeration on, so operators can run without copying Ollama addresses into `fleet.yaml`. The example `fleet.yaml` MAY be empty `nodes: []` or retain optional pins (id, labels, static capacity) without URLs. CI deploy MUST still never write or mutate `fleet.yaml`. The stack MAY mount the host Tailscale local API socket for peer enumeration; if the socket is absent, discovery MUST soft-fail that source as specified in `host-discovery`.

#### Scenario: Example overlay discovers the LAN

- **WHEN** an operator deploys the swarm stack using the in-repo example config and an empty or pin-only `fleet.yaml`
- **THEN** the documented tunables enable discovery for `192.168.100.0/24` and Tailscale enum, and no backend `url:` is required for those hosts to join once agents are up

#### Scenario: CI still does not write fleet.yaml

- **WHEN** the swarm-deploy job runs
- **THEN** it does not create or edit `fleet.yaml` on the manager

## MODIFIED Requirements

### Requirement: One router replica and no fleet.yaml writes from CI

The deployed stack SHALL run a single router replica. CI deploy SHALL NOT write or mutate `fleet.yaml` inventory; when discovery is enabled, `fleet.yaml` MAY be empty or pin-only and remains operator-owned GitOps when pins are used. Secrets (admin token, Verda/RunPod keys, zrok) SHALL be supplied as Swarm/Docker secrets or env from the host, never committed.

#### Scenario: Stack scale is one

- **WHEN** the ollama-router stack is deployed
- **THEN** the router service desired replicas equal 1

#### Scenario: Deploy artifacts contain no live secrets

- **WHEN** stack compose, bake tags, and deploy runbooks in git are scanned
- **THEN** they contain only placeholders for secrets and registry hosts, never live tokens
