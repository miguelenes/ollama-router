## ADDED Requirements

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
