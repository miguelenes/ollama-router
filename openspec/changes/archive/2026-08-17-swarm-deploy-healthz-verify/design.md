## Context

See `proposal.md`. `.woodpecker/swarm-deploy.yml` runs on the Swarm manager agent (`role=swarm-manager`) with only `/var/run/docker.sock` mounted. The step container cannot curl the host-published `:11434` on loopback; the same constraint applies to `.woodpecker/image.yml`, which uses `docker exec` against the running container.

## Goals / Non-Goals

**Goals**

- Fail CI when stack deploy does not produce a live router task.
- Confirm the service spec carries the SHA tag from `ROUTER_TAG`, not only `latest`.

**Non-Goals**

- `/readyz` gating (503 is valid when the fleet has no healthy nodes).
- Swarm service-level `HEALTHCHECK` changes in the stack file.
- Post-deploy smoke tests against inference endpoints.

## Decisions

1. **Tag check then healthz poll** — `docker service inspect` for `:sha-<7>` in the image ref, then poll up to ~4 minutes for a running task with successful in-container `/healthz`.
2. **`docker exec` on the Swarm task** — find the container with `com.docker.swarm.service.name=ollama-router_router`; reuse the image's bundled `curl` (Dockerfile HEALTHCHECK).
3. **Failure diagnostics** — on timeout, print `docker service ps --no-trunc` and tail logs with `docker logs` on the Swarm task container ID (not `docker service logs`) before exit 1.

**Rejected:** curl from the step container to `127.0.0.1:11434` — wrong network namespace on the manager agent.

## Risks / Trade-offs

- **[Risk]** Brief gap during `stop-first` updates with no running task → **Mitigation:** poll loop tolerates empty running set until the new task starts.
- **[Trade-off]** Green deploy with `/healthz` 200 but `/readyz` 503 → accepted; operators still use readiness for traffic gates.
