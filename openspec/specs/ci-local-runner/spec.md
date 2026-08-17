# ci-local-runner Specification

## Purpose

Runs all CI/CD pipelines (Rust verify, image bake, GHCR publish, fleet-registry push, swarm deploy, and Pages) on a fleet-hosted Woodpecker CI agent so work stays on the home fleet instead of GitHub-hosted VMs.

## Requirements

### Requirement: Fleet-local jobs run on a fleet-hosted Woodpecker agent

Jobs that verify the workspace, bake the router image, push images to the fleet local registry, deploy the swarm stack, publish the router image to GHCR, or publish the docs site SHALL execute on a fleet-hosted Woodpecker CI agent (Docker backend) that can reach the local registry and the Swarm control plane. GitHub-hosted runners SHALL NOT be used for any job. Fork pull requests SHALL NOT execute on the fleet agent.

#### Scenario: Fleet registry push runs on the fleet agent

- **WHEN** the fleet-registry push job is eligible to run
- **THEN** it executes on the Woodpecker agent, with no GitHub-hosted runner involved

#### Scenario: Swarm deploy runs on the fleet agent

- **WHEN** the swarm deploy job is eligible to run
- **THEN** it executes on the same fleet agent that can reach the local registry and the Swarm control plane

#### Scenario: Verify runs on the fleet agent

- **WHEN** a default-branch push or same-repository pull request is eligible for verify
- **THEN** fmt, clippy, test, deny, and coverage execute on the fleet Woodpecker agent, with no GitHub-hosted runner involved

#### Scenario: Pages publish runs on the fleet agent

- **WHEN** the Pages pipeline is eligible to run
- **THEN** the site build and `gh-pages` publish execute on the fleet Woodpecker agent, with no GitHub-hosted runner involved

#### Scenario: Fork pull requests are not executed

- **WHEN** a pull request originates from a fork
- **THEN** no pipeline for that event is executed on the fleet agent

### Requirement: Runner enablement is gated

Fleet-registry push and swarm deploy SHALL NOT run until an explicit Woodpecker repository secret (or equivalent documented gate) enables them, so a missing or unconfigured agent does not fail every default-branch push.

#### Scenario: Gate off skips fleet push

- **WHEN** the fleet push enablement secret is unset or not `true`
- **THEN** the fleet-registry push pipeline is skipped and the verify pipeline can still pass

#### Scenario: Gate on requires a matching agent

- **WHEN** the fleet push enablement secret is `true` and a push to the default branch occurs
- **THEN** the pipeline is queued on the fleet Woodpecker agent matching the required execution environment

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
