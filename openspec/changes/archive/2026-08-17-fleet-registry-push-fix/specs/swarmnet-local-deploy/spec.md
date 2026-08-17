## ADDED Requirements

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
