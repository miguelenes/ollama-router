# swarmnet-local-deploy Specification

## Purpose

Deploys and updates the ollama-router Swarm stack on the swarmnet Docker Swarm using images from the fleet local registry only, never from GHCR on that path.

## Requirements

### Requirement: Swarm deploy images come only from the fleet local registry

The swarmnet deploy path SHALL push the router image to the fleet local registry (loopback or LAN registry host such as `127.0.0.1:5005`) and the Swarm stack SHALL reference that registry hostname for pulls. That path SHALL NOT push the deploy image to `ghcr.io` and SHALL NOT configure the stack to pull `ghcr.io/...` for the router service.

#### Scenario: Push-on-merge updates the local registry tag

- **WHEN** fleet-registry push is enabled and a change lands on the default branch
- **THEN** the fleet CI agent pushes `router` to the configured fleet registry with `latest` and the git SHA tag

#### Scenario: Stack image refs are local-registry only

- **WHEN** the Swarm stack definition for ollama-router is inspected
- **THEN** the router service image reference uses the fleet local registry host, not `ghcr.io`

### Requirement: Deploy is separately gated from registry push

Updating the live Swarm stack SHALL require an explicit deploy enablement gate in addition to the fleet-registry push gate, so images can be published to the local registry before live traffic is moved.

#### Scenario: Push enabled without deploy leaves the stack unchanged

- **WHEN** fleet-registry push is enabled and deploy is disabled
- **THEN** images are pushed to the local registry and no stack update is attempted

#### Scenario: Both gates enabled update the stack

- **WHEN** both push and deploy gates are enabled and the push job succeeds
- **THEN** the deploy job updates or recreates the ollama-router stack to the new local-registry digest/tag

### Requirement: One router replica and no fleet.yaml writes from CI

The deployed stack SHALL run a single router replica. CI deploy SHALL NOT write or mutate `fleet.yaml` inventory; when discovery is enabled, `fleet.yaml` MAY be empty or pin-only and remains operator-owned GitOps when pins are used. Secrets (admin token, Verda/RunPod keys, zrok) SHALL be supplied as Swarm/Docker secrets or env from the host, never committed.

#### Scenario: Stack scale is one

- **WHEN** the ollama-router stack is deployed
- **THEN** the router service desired replicas equal 1

#### Scenario: Deploy artifacts contain no live secrets

- **WHEN** stack compose, bake tags, and deploy runbooks in git are scanned
- **THEN** they contain only placeholders for secrets and registry hosts, never live tokens

### Requirement: Swarm example enables discovery without listing backend IPs

The swarm deploy example tunables SHALL show `discovery.enabled` with the home LAN CIDR (`192.168.100.0/24`) and Tailscale peer enumeration on, so operators can run without copying Ollama addresses into `fleet.yaml`. The example `fleet.yaml` MAY be empty `nodes: []` or retain optional pins (id, labels, static capacity) without URLs. CI deploy MUST still never write or mutate `fleet.yaml`. The stack MAY mount the host Tailscale local API socket for peer enumeration; if the socket is absent, discovery MUST soft-fail that source as specified in `host-discovery`.

#### Scenario: Example overlay discovers the LAN

- **WHEN** an operator deploys the swarm stack using the in-repo example config and an empty or pin-only `fleet.yaml`
- **THEN** the documented tunables enable discovery for `192.168.100.0/24` and Tailscale enum, and no backend `url:` is required for those hosts to join once agents are up

#### Scenario: CI still does not write fleet.yaml

- **WHEN** the swarm-deploy job runs
- **THEN** it does not create or edit `fleet.yaml` on the manager

### Requirement: Fleet local registry bootstrap is defined in-repo

The swarm deploy runbook SHALL reference a committed compose file under `deploy/swarm/` that starts the fleet local registry (`registry:2` or equivalent) on the CI agent host (where Woodpecker `fleet-push` runs) with loopback and documented LAN bind ports. Operators SHALL be able to bootstrap the registry without copying ad-hoc compose from chat or an uncommitted local file.

#### Scenario: Runbook points at committed registry compose

- **WHEN** an operator follows `deploy/swarm/README.md` bootstrap step zero
- **THEN** the documented `docker compose -f deploy/swarm/fleet-registry.compose.yaml up -d` command uses a file that exists in the repository

#### Scenario: Registry exposes loopback and LAN bind

- **WHEN** `deploy/swarm/fleet-registry.compose.yaml` is applied on the CI agent host
- **THEN** the registry listens on loopback `:5005` and on a configurable LAN bind documented in the compose header comments for swarm nodes that pull by LAN IP

### Requirement: Split-agent fleets replicate to the manager registry

When the Woodpecker CI push agent and the Swarm manager run on different hosts, the fleet-push pipeline SHALL support publishing the same router image tags to the manager's fleet local registry so swarm deploy can pull them without a manual replicate step. Replication SHALL be controlled by an optional Woodpecker repository secret (`FLEET_REGISTRY_REPLICA`); when unset, behavior SHALL remain a single push to `FLEET_REGISTRY` only.

#### Scenario: Replica secret publishes SHA tags on the manager registry

- **WHEN** fleet-registry push is enabled, `FLEET_REGISTRY` is the CI agent loopback registry, `FLEET_REGISTRY_REPLICA` is the Swarm manager registry LAN host (for example `192.168.100.5:5005`), and a default-branch push runs fleet-push
- **THEN** `ollama-router:latest` and `ollama-router:sha-<git>` are visible on both the primary push registry and the replica registry API

#### Scenario: Absent replica secret keeps single-registry push

- **WHEN** fleet-registry push is enabled and `FLEET_REGISTRY_REPLICA` is unset
- **THEN** fleet-push publishes only to `FLEET_REGISTRY` and does not fail because no replica is configured

#### Scenario: Swarm deploy pulls replica-published SHA

- **WHEN** both push and deploy gates are enabled, `FLEET_REGISTRY_REPLICA` points at the manager registry, `DEPLOY_REGISTRY` is manager loopback `127.0.0.1:5005`, and fleet-push succeeds on the default branch
- **THEN** the subsequent swarm-deploy job can update the stack to `sha-<git>` without a manual image copy

### Requirement: Fleet local registry remains push-capable

The fleet local registry used for Woodpecker `fleet-push` and Swarm deploy pulls SHALL run as a standard Distribution filesystem registry and SHALL NOT be configured as a pull-through proxy (`proxy.remoteurl` / `REGISTRY_PROXY_REMOTEURL`) for an upstream such as Docker Hub. Hub image caching SHALL use a separate registry instance or an existing fleet mirror endpoint documented for operators.

#### Scenario: Fleet-push succeeds against the local registry

- **WHEN** the fleet local registry is running per `deploy/swarm/fleet-registry.compose.yaml` and Woodpecker fleet-push publishes `ollama-router:latest` and `ollama-router:sha-<git>`
- **THEN** both tags are accepted via `docker push` and are visible on the registry API at the configured push host

#### Scenario: Hub pull-through cache is not the fleet push target

- **WHEN** an operator inspects the committed fleet local registry bootstrap for swarm deploy
- **THEN** it does not enable Distribution pull-through proxy mode on the `:5005` fleet push registry

#### Scenario: Hub cache remains a separate role

- **WHEN** fleet hosts need accelerated Docker Hub pulls (for example via daemon `registry-mirrors`)
- **THEN** documentation describes the Hub cache as a separate registry process or fleet mirror hostname, not as a merged replacement for the push-capable fleet registry on `:5005`

#### Scenario: Replica registry is push-capable

- **WHEN** `FLEET_REGISTRY_REPLICA` is set for split-agent fleet-push
- **THEN** that host is a filesystem push registry (same constraints as the primary fleet registry) and is not configured as a pull-through proxy

### Requirement: Swarm deploy verifies the updated router service

When both fleet-registry push and swarm deploy gates are enabled and the deploy step runs, the swarm-deploy pipeline SHALL confirm the `ollama-router` router service references the git SHA tag from the deploy step and SHALL poll `GET /healthz` on the running Swarm task before the job completes successfully. The pipeline SHALL exit non-zero if the service image does not include the expected SHA tag or if `/healthz` does not succeed within a bounded timeout. Verification SHALL use the router process liveness endpoint (`/healthz`), not fleet readiness (`/readyz`).

#### Scenario: Deploy job confirms the pinned SHA tag

- **WHEN** both push and deploy gates are enabled, fleet-push succeeds, and swarm-deploy runs with `ROUTER_TAG=sha-<git>`
- **THEN** the pipeline inspects the router service spec and requires the image reference to include `:sha-<git>` before polling health

#### Scenario: Deploy job fails when the service image lacks the pinned SHA tag

- **WHEN** swarm-deploy runs with `ROUTER_TAG=sha-<git>` but the router service spec image reference does not include `:sha-<git>`
- **THEN** the swarm-deploy step exits non-zero before or instead of a successful health poll

#### Scenario: Deploy job succeeds when healthz responds

- **WHEN** swarm-deploy has updated the stack and a running Swarm task for the router service is available
- **THEN** the pipeline probes `GET /healthz` on the task (for example via in-container curl through `docker exec`) and completes successfully when the endpoint returns success

#### Scenario: Deploy job fails when healthz never becomes ready

- **WHEN** swarm-deploy updates the stack but no running task responds successfully to `/healthz` within the pipeline timeout
- **THEN** the swarm-deploy step exits non-zero and emits service/task diagnostics so the failed deploy is visible in CI
