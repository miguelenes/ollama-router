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

The deployed stack SHALL run a single router replica. CI deploy SHALL NOT write or mutate `fleet.yaml` inventory; inventory remains GitOps / operator-owned. Secrets (admin token, Verda/RunPod keys, zrok) SHALL be supplied as Swarm/Docker secrets or env from the host, never committed.

#### Scenario: Stack scale is one

- **WHEN** the ollama-router stack is deployed
- **THEN** the router service desired replicas equal 1

#### Scenario: Deploy artifacts contain no live secrets

- **WHEN** stack compose, bake tags, and deploy runbooks in git are scanned
- **THEN** they contain only placeholders for secrets and registry hosts, never live tokens
