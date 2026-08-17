# ollama-router on swarmnet — fleet local registry deploy

This directory is the **fleet deploy** path for the ollama-router Swarm stack.
It is deliberately separate from the public GHCR package: swarmnet deploys
pull **only** from the fleet local registry, and CI on this path never pushes
to or pulls from `ghcr.io` (enforced by the `swarm-stack-refs` step of
`.woodpecker/verify.yml`).

## Two registries, two jobs — don't mix them

| Path | Image source | Pipeline | Purpose |
| --- | --- | --- | --- |
| Public package | `ghcr.io/miguelenes/ollama-router` (`latest`, `edge`, `sha-*`, semver) | `.woodpecker/publish-ghcr.yml` | Public consumers pull the tagged package |
| Fleet deploy | `127.0.0.1:5005/ollama-router` (or LAN registry) | `.woodpecker/fleet-push.yml` + `.woodpecker/swarm-deploy.yml` | Swarm stack updates from the local registry |

- **Public consumers:** `docker pull ghcr.io/miguelenes/ollama-router:latest`
  (or an `edge` / `sha-*` / semver tag). Because the repository is public,
  builds carry provenance attestations.
- **Fleet deploy:** the stack in this directory references
  `${DEPLOY_REGISTRY:-127.0.0.1:5005}/ollama-router`. CI pushes to the fleet
  local registry (and optionally replicates to the manager registry); the stack
  never pulls `ghcr.io/...`.

## Architecture

```text
push on main (gates on)                 deploy (gate on)
Woodpecker agent  ── publish ──►  ghcr.io/miguelenes/ollama-router   (public)
Woodpecker agent  ── push ──►  :5005 fleet registry (primary)
              └── replicate ──►  manager LAN :5005 (optional FLEET_REGISTRY_REPLICA)
Woodpecker agent  ── stack ──►  docker stack deploy ollama-router (local only)
```

## Registry roles (push vs Hub cache)

This is **not** the same as **Two registries, two jobs** above (GHCR vs fleet
deploy). Within the fleet LAN you still run **two Distribution registry
roles** — do not merge them into one container:

| Role | Typical host / port | Push? | Purpose |
| --- | --- | --- | --- |
| Fleet push registry | `127.0.0.1:5005` (CI agent), NAS LAN `192.168.100.5:5005`, optional Tailscale `registry.bicorn-beta.ts.net` | **Yes** | Woodpecker `fleet-push` publishes `ollama-router:*`; Swarm pulls via `DEPLOY_REGISTRY` |
| Hub pull-through cache | NAS `:9999`, Tailscale `cache.bicorn-beta.ts.net` | **No** | Docker daemon `registry-mirrors` for Docker Hub pulls |

**Why not one container?** Distribution [pull-through cache
mode](https://distribution.github.io/distribution/recipes/mirror/) does **not
support push**. Fleet-push requires `docker push` to `:5005/ollama-router`.
Enabling `REGISTRY_PROXY_REMOTEURL` on the fleet registry would break or
undefined-push behavior. Hub caching stays a **separate** registry process.

**`FLEET_REGISTRY_REPLICA`** (see [Push host vs pull host](#push-host-vs-pull-host-split-agents))
replicates fleet tags to a **second push-capable** registry (for example NAS
LAN `:5005`). It is **not** the Hub cache — never set it to
`cache.bicorn-beta.ts.net` or NAS `:9999`.

Example daemon mirror (Hub only — not fleet deploy):

```json
{
  "registry-mirrors": ["https://cache.bicorn-beta.ts.net"]
}
```

On the NAS manager, fleet `:5005` and Hub cache `:9999` are often the
Portainer `registry` stack (`registry` + Tailscale sidecar + `cache` +
sidecar) — out-of-repo fleet infra, same two-role split.

## Fleet CI agent (Woodpecker)

All pipelines run on the fleet-hosted Woodpecker agent (Docker backend) —
see `deploy/woodpecker/README.md` for provisioning, repo activation, and
secrets. The agent host must:

- reach the fleet registry (loopback `127.0.0.1:5005` or LAN),
- trust it as an insecure registry — add to the Docker daemon:

  ```json
  {
    "insecure-registries": [
      "127.0.0.1:5005",
      "192.168.100.135:5005",
      "192.168.100.5:5005"
    ]
  }
  ```

  Include the Swarm manager registry LAN (`FLEET_REGISTRY_REPLICA`) when the
  CI agent replicates to a remote manager registry. Then restart Docker.
- for `SWARM_DEPLOY_ENABLED`: the docker socket it mounts must be the Swarm
  manager's (or `DOCKER_HOST` must point at it).

**Fleet-push buildx (loopback `FLEET_REGISTRY`).** When `FLEET_REGISTRY` is
loopback (`127.0.0.1:5005`), `.woodpecker/fleet-push.yml` uses
`docker buildx build --builder default` so the push reaches the host registry.
A docker-container builder such as `swarmos` isolates `127.0.0.1` inside
BuildKit and push fails with connection refused. Remote push hostnames
(Tailscale/LAN DNS) may work with either builder.

**Fleet-agent reachability.** The registry is on the fleet LAN
(loopback), so the push must originate on a host that can reach it — the
Woodpecker agent that runs `.woodpecker/fleet-push.yml`.

## Enablement gates

Woodpecker **repository secrets** (all default off — absent secret = off; a
missing gate never fails a default-branch push):

| Secret | Default | Effect |
| --- | --- | --- |
| `FLEET_REGISTRY_PUSH_ENABLED` | off | `true` runs the fleet-registry push pipeline |
| `FLEET_REGISTRY` | `127.0.0.1:5005` | Primary push target on the CI agent |
| `FLEET_REGISTRY_REPLICA` | off | Optional manager registry LAN (e.g. `192.168.100.5:5005`) |
| `DEPLOY_REGISTRY` | = `FLEET_REGISTRY` | Pull host the swarm nodes use |
| `SWARM_DEPLOY_ENABLED` | off | `true` (with push enabled) updates the stack |

Set them with `woodpecker-cli` (see `deploy/woodpecker/README.md`):

```bash
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name FLEET_REGISTRY_PUSH_ENABLED --value true --event push
# ...and SWARM_DEPLOY_ENABLED when ready; remove a secret to turn it off
```

### Loopback vs LAN registry (when to set `DEPLOY_REGISTRY`)

- Agent and all swarm nodes on the **same host** (single-node swarm):
  `FLEET_REGISTRY=127.0.0.1:5005` and leave `DEPLOY_REGISTRY` unset.
- Swarm nodes pull over the network: keep the push on loopback if the agent
  is on the registry host, but set
  `DEPLOY_REGISTRY=192.168.1.50:5005` (the registry's LAN IP) so every node
  resolves the same registry. `127.0.0.1` in the stack would mean "this
  node's loopback" on each swarm node — broken for multi-node pulls.
- The stack pins the router to the manager (`node.role == manager`) because
  `fleet.yaml` is bind-mounted there.

### Push host vs pull host (split agents)

When the Woodpecker CI agent (`role=ci`, usually the desktop/registry host)
pushes but the Swarm manager agent (`role=swarm-manager`, usually the NAS)
runs `docker stack deploy`, set **three** registry secrets:

| Secret | Role | Example (this fleet) |
| --- | --- | --- |
| `FLEET_REGISTRY` | Where `fleet-push` publishes on the CI agent | `127.0.0.1:5005` (desktop loopback registry) |
| `FLEET_REGISTRY_REPLICA` | Optional second publish target (manager registry LAN) | `192.168.100.5:5005` (NAS registry from the LAN) |
| `DEPLOY_REGISTRY` | Where swarm nodes pull | `127.0.0.1:5005` on the NAS manager |

Validated split on this fleet: desktop `fleet-push` publishes to desktop
loopback **and** replicates to `192.168.100.5:5005`; the NAS manager deploys
with `DEPLOY_REGISTRY=127.0.0.1:5005`. Do not point desktop `FLEET_REGISTRY`
at the NAS loopback — that is the manager's `127.0.0.1`, not the desktop host.
The CI agent daemon must trust `FLEET_REGISTRY_REPLICA` in
`insecure-registries` when it uses HTTP.

## Bootstrap (first deploy)

0. **Start the fleet local registry** (once per agent host):
   ```bash
   docker compose -f deploy/swarm/fleet-registry.compose.yaml up -d
   curl -sf http://127.0.0.1:5005/v2/
   ```
   Add `127.0.0.1:5005` and the LAN bind (e.g. `192.168.100.135:5005`) to the
   Docker daemon `insecure-registries`, then restart Docker.

1. **Create secrets** (never commit):
   ```bash
   printf '%s' "$ADMIN_TOKEN" | docker secret create ollama-router-admin-token -
   # optional: VERDA_CLIENT_ID / VERDA_CLIENT_SECRET / RUNPOD_API_KEY / zrok
   #   printf '%s' "$VERDA_CLIENT_ID"     | docker secret create verda-client-id -
   #   printf '%s' "$VERDA_CLIENT_SECRET" | docker secret create verda-client-secret -
   #   printf '%s' "$RUNPOD_API_KEY"      | docker secret create runpod-api-key -
   ```
   The stack declares `ollama-router-admin-token` as `external: true`; add
   others to the `secrets:` list and the service `secrets:` entry if needed.

2. **Place inventory** on the manager:
   ```bash
   sudo install -m 0444 -o root -g root fleet.yaml /etc/ollama-router/fleet.yaml
   # optional tunables overlay:
   sudo install -m 0440 -o root -g root router.config.example.yaml /etc/ollama-router/config.yaml
   ```
   `fleet.yaml` stays GitOps / operator-owned. **CI never writes it.**

3. **Validate the stack file** (from the repo root, requires Docker):
   ```bash
   docker compose -f deploy/swarm/ollama-router.stack.yml config --quiet
   ```

4. **Push the image once** (optional manual warm-up; CI can do it):
   ```bash
   docker buildx bake -f docker-bake.hcl router \
     --set "router.tags=127.0.0.1:5005/ollama-router:latest" \
     --set "router.tags=127.0.0.1:5005/ollama-router:sha-$(git rev-parse --short HEAD)" \
     --push
   ```

5. **Deploy manually once** to prove the stack:
   ```bash
   DEPLOY_REGISTRY="${DEPLOY_REGISTRY:-127.0.0.1:5005}" \
   ROUTER_TAG="sha-$(git rev-parse --short HEAD)" \
   docker stack deploy --prune -c deploy/swarm/ollama-router.stack.yml ollama-router
   curl -fsS http://127.0.0.1:11434/healthz
   ```

6. **Enable the gates** (Woodpecker secrets): first
   `FLEET_REGISTRY_PUSH_ENABLED=true` (verify the image appears on `:5005`
   and the stack is unchanged), then `SWARM_DEPLOY_ENABLED=true`.

## CI update flow (after bootstrap)

```bash
# every main push with gates on
push:   fleet-push -> primary :5005 + optional replica (FLEET_REGISTRY_REPLICA)
deploy: swarm-deploy runs docker stack deploy with ROUTER_TAG=sha-<7>,
        verifies the service image tag, then polls /healthz in the task
```


`docker stack deploy` updates only changed services; the update config uses
`order: stop-first` (never two replicas sharing FleetState) and
`failure_action: rollback`.

## Rollback

Point the stack back at a previous SHA tag from the local registry:

```bash
# turn the deploy gate off first (remove the SWARM_DEPLOY_ENABLED secret), then:
ROUTER_TAG=sha-<previous> DEPLOY_REGISTRY="${DEPLOY_REGISTRY:-127.0.0.1:5005}" \
  docker stack deploy -c deploy/swarm/ollama-router.stack.yml ollama-router
curl -fsS http://127.0.0.1:11434/healthz
```

The previous tag is still on the local registry (`latest` moved, `sha-*`
tags are immutable), so no rebuild is needed. Re-add
`SWARM_DEPLOY_ENABLED` once verified. Disabling the gate does not touch the
running stack.

## Security notes

- No live tokens, admin bearer, zrok share tokens, or cloud keys in this
  directory or in git. Secrets are Swarm secrets, host env at deploy time,
  or Woodpecker repo secrets.
- The admin API is fail-closed: with no token the router returns 403 on
  `/router/v1/*`.
- `deploy/swarm/` is scanned by CI (`swarm-stack-refs` step) for `ghcr.io`
  references — keep it local-registry only.
