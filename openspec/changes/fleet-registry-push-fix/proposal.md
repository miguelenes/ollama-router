## Why

The Woodpecker `fleet-push` pipeline failed on the desktop CI agent when `FLEET_REGISTRY=127.0.0.1:5005`: buildx used the default `swarmos` builder (docker-container driver), so `127.0.0.1` inside BuildKit pointed at the builder container, not the host registry on `:5005`. Even after fixing the builder, a push to the desktop registry alone does not populate the Swarm manager registry on the NAS — swarm nodes pull from manager loopback `127.0.0.1:5005`, which is a different host than the desktop CI agent.

## What Changes

- Pin the fleet-push pipeline to a buildx builder whose network namespace can reach `FLEET_REGISTRY` on the Woodpecker agent host (the `default` docker driver, not an isolated docker-container builder).
- Commit `deploy/swarm/fleet-registry.compose.yaml` (registry:2 on loopback and LAN bind) so bootstrap matches the runbook.
- Add optional **`FLEET_REGISTRY_REPLICA`**: when set, fleet-push tags and pushes the same build to a second registry (typically the Swarm manager LAN registry, e.g. `192.168.100.5:5005`) in one buildx invocation so `swarm-deploy` can pull `sha-*` without a manual replicate step.
- Document the builder constraint, registry bootstrap, and three-secret split in `deploy/swarm/README.md` and Woodpecker docs: `FLEET_REGISTRY` (CI agent push), `FLEET_REGISTRY_REPLICA` (optional manager registry LAN), `DEPLOY_REGISTRY` (stack pull host on the manager, usually manager loopback).

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `ci-local-runner`: fleet-registry push on the Woodpecker agent must reach the configured host registry when the push gate is on.
- `swarmnet-local-deploy`: fleet local registry provisioning is defined in-repo and referenced by the swarm bootstrap runbook.

## Impact

- `.woodpecker/fleet-push.yml` — `--builder default` (or equivalent documented choice) for host-reachable push.
- `deploy/swarm/fleet-registry.compose.yaml` — new committed compose for the fleet registry.
- `deploy/swarm/README.md` — builder + insecure-registry bootstrap notes, push-vs-pull registry secret guidance (no live secrets).
- No Rust crate or proxy API changes; honest-fleet contract unchanged.
