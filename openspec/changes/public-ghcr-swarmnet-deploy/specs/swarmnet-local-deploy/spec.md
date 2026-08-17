## Purpose

Deploys and updates the ollama-router Swarm stack on the swarmnet Docker Swarm using images from the fleet local registry only, never from GHCR on that path.

## MODIFIED Requirements

### Requirement: Swarm deploy images come only from the fleet local registry

The swarmnet deploy path SHALL push the router image to the fleet local registry (loopback or LAN registry host such as `127.0.0.1:5005`) and the Swarm stack SHALL reference that registry hostname for pulls. That path SHALL NOT push the deploy image to `ghcr.io` and SHALL NOT configure the stack to pull `ghcr.io/...` for the router service.

#### Scenario: Push-on-merge updates the local registry tag

- **WHEN** fleet-registry push is enabled and a change lands on the default branch
- **THEN** the fleet CI agent pushes `router` to the configured fleet registry with `latest` and the git SHA tag

#### Scenario: Stack image refs are local-registry only

- **WHEN** the Swarm stack definition for ollama-router is inspected
- **THEN** the router service image reference uses the fleet local registry host, not `ghcr.io`
