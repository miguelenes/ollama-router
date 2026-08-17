## Purpose

Runs Docker image build, fleet-registry push, and swarm deploy GitHub Actions jobs on a self-hosted local runner so heavy work stays on the home fleet instead of GitHub-hosted VMs.

## ADDED Requirements

### Requirement: Docker push and swarm deploy jobs use a self-hosted runner

Jobs that push images to the fleet local registry or deploy the swarm stack SHALL run on a self-hosted runner whose labels include `self-hosted` and a stable fleet label for this project (for example `ollama-router` or a shared fleet label documented in the runbook). Pure verify jobs (fmt, clippy, test, coverage, Pages build) MAY remain on `ubuntu-latest`.

#### Scenario: Fleet registry push runs on the local runner

- **WHEN** the fleet-registry push job is eligible to run
- **THEN** its `runs-on` selects the self-hosted runner labels, not `ubuntu-latest`

#### Scenario: Swarm deploy runs on the local runner

- **WHEN** the swarm deploy job is eligible to run
- **THEN** it executes on the same class of self-hosted runner that can reach the local registry and Swarm/Portainer control plane

### Requirement: Runner enablement is gated

Fleet-registry push and swarm deploy SHALL NOT run until an explicit repository Actions variable (or equivalent documented gate) enables them, so a missing runner does not fail every default-branch push.

#### Scenario: Gate off skips fleet push

- **WHEN** the fleet push enablement variable is unset or not `true`
- **THEN** the fleet-registry push job is skipped and the verify workflow can still pass

#### Scenario: Gate on requires a matching runner

- **WHEN** the fleet push enablement variable is `true` and a push to the default branch occurs
- **THEN** the job is queued for a runner matching the required labels
