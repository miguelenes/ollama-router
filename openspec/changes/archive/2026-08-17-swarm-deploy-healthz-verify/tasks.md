## 1. Pipeline verification

- [x] 1.1 After `docker stack deploy`, inspect `ollama-router_router` and fail if the service image ref does not include `ROUTER_TAG` (`sha-<git>`)
- [x] 1.2 Poll the running Swarm task with in-container `curl` to `GET /healthz`; exit non-zero on timeout; print `docker service ps --no-trunc` and tail logs with `docker logs` on the Swarm task container ID on failure
- [x] 1.3 Document the verification step in `deploy/swarm/README.md` and `deploy/woodpecker/README.md`
