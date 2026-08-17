## ADDED Requirements

### Requirement: Fleet registry push reaches the configured host registry

When the fleet-registry push gate is enabled, the Woodpecker fleet-push pipeline SHALL push the router image to the configured `FLEET_REGISTRY` host as seen from the agent host network namespace (for example loopback `127.0.0.1:5005` or a LAN registry IP). The push path SHALL NOT rely on a build environment where `127.0.0.1` refers to an isolated builder container instead of the agent host.

#### Scenario: Loopback registry push succeeds on the agent

- **WHEN** the fleet push gate is `true`, `FLEET_REGISTRY` is `127.0.0.1:5005`, the fleet local registry is listening on the agent host, and a default-branch push runs the fleet-push pipeline
- **THEN** the pipeline pushes `ollama-router:latest` and `ollama-router:sha-<git>` to the host registry and the tags are visible via the registry API on `:5005`

#### Scenario: Isolated builder localhost does not satisfy push

- **WHEN** a build step uses a container-isolated builder whose `127.0.0.1` is not the agent host
- **THEN** the fleet-push pipeline MUST NOT use that builder as the sole push path for a loopback `FLEET_REGISTRY` without additional host networking configuration

### Requirement: Optional replica registry push from the CI agent

When `FLEET_REGISTRY_REPLICA` is set, the fleet-push pipeline SHALL include the replica registry in the same buildx push (same digest, `latest` and `sha-<git>` tags). The CI agent Docker daemon SHALL be documented as needing `insecure-registries` for the replica LAN host when it uses HTTP.

#### Scenario: Replica push uses the default docker builder

- **WHEN** `FLEET_REGISTRY_REPLICA` is set to a LAN registry reachable from the CI agent and fleet-push runs
- **THEN** the pipeline uses the same host-reachable buildx builder as the primary push (for example `--builder default`) so both registries receive layers in one build

#### Scenario: Replica push failure fails the pipeline

- **WHEN** `FLEET_REGISTRY_REPLICA` is set but the CI agent cannot reach or authenticate to the replica registry
- **THEN** the fleet-push step exits non-zero so swarm-deploy does not run against a missing SHA tag
