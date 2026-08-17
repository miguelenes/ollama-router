## Context

See `proposal.md` — Why. Woodpecker `fleet-push` already runs on the desktop CI agent (`role=ci`) and pushes to `FLEET_REGISTRY` (often `127.0.0.1:5005` or `registry.bicorn-beta.ts.net` for cross-host push). Pipeline 23 failed with connection refused when the desktop agent tried `127.0.0.1:5005` while the default buildx builder was `swarmos` (docker-container driver): BuildKit's `127.0.0.1` is inside the builder container, not the host where the registry listens. `deploy/swarm/README.md` already documents `fleet-registry.compose.yaml`, but the file was not in git.

## Goals / Non-Goals

**Goals**

- Make fleet-push reliably reach the host-local registry when `FLEET_REGISTRY` is loopback on the agent.
- Commit the registry compose referenced by the swarm runbook.
- Document the buildx builder constraint for operators.

**Non-Goals**

- Changing GHCR publish (`publish-ghcr.yml`) — that pipeline may keep using a container builder with remote registry hosts.
- Moving the fleet registry onto Swarm or NAS — it stays on the CI agent host for push simplicity.
- Rust router behavior, proxy API, or honest-fleet contract changes.
- Replacing the existing dual-secret pattern (`FLEET_REGISTRY` for push host vs `DEPLOY_REGISTRY` for swarm pull).

## Decisions

1. **Use `--builder default` in `fleet-push.yml`**  
   The `default` buildx builder uses the docker driver and shares the agent host network namespace, so `127.0.0.1:5005` reaches the host registry.  
   **Rejected:** `network=host` on a docker-container builder — heavier and duplicates the docker driver outcome for this pipeline. **Rejected:** hard-coding LAN IP in the pipeline — breaks loopback-only setups; secret `FLEET_REGISTRY` stays authoritative.

2. **Commit `deploy/swarm/fleet-registry.compose.yaml`**  
   `registry:2` with loopback `127.0.0.1:5005:5000` and a LAN bind (example `192.168.100.135:5005:5000`) in comments; operators adjust the LAN IP for their fleet. Persistent volume for tags.  
   **Rejected:** documenting only swarmnet's existing registry without an in-repo compose — runbook already promised a repo file.

3. **Document insecure-registry + builder notes in README**  
   Extend bootstrap step zero and fleet-push header comments: daemon `insecure-registries` for HTTP registry, and why fleet-push pins `default` builder.  
   **Rejected:** a Woodpecker-only doc outside `deploy/swarm/` — operators look at the swarm runbook first.

4. **Optional `FLEET_REGISTRY_REPLICA` for split push/pull hosts**  
   When the CI agent (`role=ci`, desktop) and Swarm manager (`role=swarm-manager`, NAS) are different machines, a single push to desktop `127.0.0.1:5005` does not land on the manager registry. Fleet-push adds extra `-t` tags when `FLEET_REGISTRY_REPLICA` is set (validated: `192.168.100.5:5005`, the NAS registry LAN). One buildx `--push` publishes to both registries. Absent secret = no replica (single-host fleets unchanged). The CI agent daemon must trust the replica host in `insecure-registries`.  
   **Rejected:** pointing desktop `FLEET_REGISTRY` at NAS loopback — that is the manager's `127.0.0.1`, not the desktop host. **Rejected:** a second build/push step — duplicates work; multi-tag single push is sufficient.

5. **Leave `FLEET_REGISTRY=registry.bicorn-beta.ts.net` (or other Tailscale/LAN hostname) as a valid primary push target**  
   When push targets a remote hostname, either builder may work; loopback is the failure mode decision 1 fixes. `FLEET_REGISTRY_REPLICA` remains optional for the manager LAN when primary push stays on the desktop registry.

## Risks / Trade-offs

- **[Risk]** `default` builder lacks buildkit cache features of `swarmos` → **Mitigation:** keep `--cache-from/--cache-to type=local` on fleet-push; accept slower cold builds on the agent.
- **[Risk]** Operator recreates `swarmos` as default builder → **Mitigation:** explicit `--builder default` in pipeline; README callout.
- **[Risk]** LAN bind IP in compose is fleet-specific → **Mitigation:** comment that operators edit the second port mapping; do not commit live secrets.

## Migration Plan

1. Merge compose + pipeline + README changes.
2. On the CI agent: ensure `fleet-registry.compose.yaml` is up and daemon trusts `:5005`.
3. Re-run fleet-push with gate on; confirm tags on `:5005` and (if deploy gate on) swarm stack updates.
4. Rollback: revert `--builder default` only if a documented alternative host-network builder is configured; previous pipeline failed on loopback regardless.

## Open Questions

_(none — scope is CI/runbook only)_
