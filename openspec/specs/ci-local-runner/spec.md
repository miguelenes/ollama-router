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
