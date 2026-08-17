## Context

See `proposal.md` — Why. On this fleet:

| Role | Host / port | Image | Purpose |
| --- | --- | --- | --- |
| Fleet local registry | Desktop `127.0.0.1:5005`, NAS `192.168.100.5:5005`; optional Tailscale `registry.bicorn-beta.ts.net` (NAS sidecar) | `registry:2` in-repo compose; NAS may run `registry:3` | Woodpecker push + Swarm pull for `ollama-router:*` |
| Hub pull-through cache | NAS `:9999`, Tailscale `cache.bicorn-beta.ts.net` | `registry:3` + `REGISTRY_PROXY_REMOTEURL` (separate process) | Docker daemon `registry-mirrors` for Docker Hub pulls |

Do not conflate the two Tailscale hostnames: **`registry.bicorn-beta.ts.net`** serves the fleet push registry (optional remote push/pull of `ollama-router` tags); **`cache.bicorn-beta.ts.net`** is the Hub mirror only and must not receive `fleet-push`.

`deploy/swarm/fleet-registry.compose.yaml` in-repo bootstraps the first role on the **CI agent host** only: a **single push-only** `registry:2` container with no proxy env. On the NAS manager, fleet `:5005` is typically the Portainer `registry` service (same role, different compose). The NAS stack runs **four** containers: `registry`, `registry-sidecar`, `cache`, `cache-sidecar`.

## Goals / Non-Goals

**Goals**

- Decide whether fleet push registry and Hub cache can share one Distribution process/container.
- Document the supported topology for operators and CI (`fleet-push` must keep working).

**Non-Goals**

- Custom registry images, nginx path routing, or a third registry product.
- Merging Tailscale sidecars into the registry image (still separate network namespace concerns).
- Moving the NAS Portainer stack into this repo in this change.
- Changing `registry-mirrors` fleet-wide (daemon.json on each host stays operator-owned).
- Changing Woodpecker pipelines, `FLEET_REGISTRY` / `FLEET_REGISTRY_REPLICA` / `DEPLOY_REGISTRY`, or buildx `--builder default` (covered by archived fleet-registry-push-fix).

## Decisions

1. **Do not merge push registry and Hub pull-through cache into one Distribution instance**  
   Distribution documents that **push to a pull-through cache is not supported** (`proxy.remoteurl` mode). Fleet-push requires `docker push` to `:5005/ollama-router`. Enabling `REGISTRY_PROXY_REMOTEURL` on the fleet registry would break or undefined-push behavior.  
   **Rejected:** one `registry:3` with both `filesystem` root and `REGISTRY_PROXY_REMOTEURL` — upstream docs treat proxy as registry-wide mode, not per-repository routing. **Rejected:** dropping Hub cache and pulling Hub only via mirror hostname on a second service — that is the current correct pattern, not a merge.

2. **Keep `fleet-registry.compose.yaml` as one push-only service**  
   No proxy env vars. Optional comment pointing to Hub cache as a separate operator stack.  
   **Rejected:** adding a second service to the same compose file and calling it "merged" — reduces file count but not container count; acceptable as documentation-only sibling service example only if operators want copy-paste (defer to README prose instead).

3. **Sidecars are not mergeable with registry without a custom image**  
   Tailscale `network_mode: service:registry` requires a second container per published hostname (`registry.bicorn-beta.ts.net` vs `cache.bicorn-beta.ts.net`). Collapsing to one sidecar would lose separate MagicDNS/Serve endpoints unless Serve config multiplexes paths on one hostname — out of scope and breaks existing daemon mirror URL.  
   **Rejected:** four → two containers by dropping sidecars — loses Tailscale Serve for remote pulls.

4. **Desktop CI does not need a local Hub cache container**  
   The desktop daemon already uses `registry-mirrors: ["https://cache.bicorn-beta.ts.net"]` for Hub pulls; `fleet-registry.compose.yaml` only needs to accept fleet pushes. No second container required on the CI agent.

5. **`FLEET_REGISTRY_REPLICA` is replication, not cache merge**  
   Split-agent fleet-push may publish the same tags to a second push-capable registry (validated: NAS LAN `192.168.100.5:5005`). That replica MUST stay a filesystem push registry — never the Hub pull-through cache hostname (`cache.bicorn-beta.ts.net`) or NAS `:9999` proxy instance.  
   **Rejected:** using `FLEET_REGISTRY_REPLICA=cache.bicorn-beta.ts.net` — proxy mode cannot accept fleet pushes.

## Risks / Trade-offs

- **[Risk]** Operator enables proxy on `:5005` thinking it saves disk → fleet-push fails → **Mitigation:** README + compose header state push-only; spec forbids proxy on fleet registry.
- **[Risk]** Desire for fewer containers on NAS persists → **Mitigation:** document minimum viable split (2 registry processes + optional 2 sidecars); true single-container merge is not supported by Distribution for this use case.
- **[Trade-off]** Two registries mean two volumes and ports — acceptable; roles have incompatible Distribution modes.

## Migration Plan

1. Merge README + compose comment updates (no running stack change required).
2. No migration of existing NAS tags — `:5005` and `:9999` keep current data paths.
3. Rollback: revert docs only.

## Open Questions

_(none — Distribution push/proxy incompatibility is definitive.)_
