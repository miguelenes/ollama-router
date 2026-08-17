# ollama-router on swarmnet — fleet local registry deploy

This directory is the **fleet deploy** path for the ollama-router Swarm stack.
It is deliberately separate from the public GHCR package: swarmnet deploys
pull **only** from the fleet local registry, and CI on this path never pushes
to or pulls from `ghcr.io` (enforced by a CI grep on `deploy/swarm/`).

## Two registries, two jobs — don't mix them

| Path | Image source | Job | Purpose |
| --- | --- | --- | --- |
| Public package | `ghcr.io/miguelenes/ollama-router` (`latest`, `edge`, `sha-*`, semver) | `.github/workflows/docker.yml` (GHCR) | Public consumers pull the tagged package |
| Fleet deploy | `127.0.0.1:5005/ollama-router` (or LAN registry) | `.github/workflows/deploy-swarm.yml` | Swarm stack updates from the local registry |

- **Public consumers:** `docker pull ghcr.io/miguelenes/ollama-router:latest`
  (or an `edge` / `sha-*` / semver tag). After the repository is public,
  builds carry provenance attestations.
- **Fleet deploy:** the stack in this directory references
  `${DEPLOY_REGISTRY:-127.0.0.1:5005}/ollama-router`. There is **no
  dual-push**: the swarm deploy job pushes only to the fleet local registry
  and the stack is never configured to pull `ghcr.io/...`.

## Architecture

```text
push on main (gates on)              deploy (gate on)
ubuntu-latest GHCR  ──────────►  ghcr.io/miguelenes/ollama-router   (public)
self-hosted runner  ── push ──►  :5005 fleet registry
self-hosted runner  ── stack ──►  docker stack deploy ollama-router (local only)
```

## Self-hosted runner

Jobs that push to the fleet registry or deploy the stack run on a
self-hosted runner with labels:

```text
self-hosted,linux,ollama-router
```

Reuse an existing swarmnet runner by adding the `ollama-router` label to its
`config.toml` (then restart the runner), or register a dedicated runner
container:

```bash
cd deploy/swarm
cp runner.env.example runner.env      # fill in ACCESS_TOKEN (never commit)
docker compose --profile runner --env-file runner.env up -d
```

The runner host must:

- reach the fleet registry (loopback `127.0.0.1:5005` or LAN),
- trust it as an insecure registry — add to the Docker daemon:

  ```json
  { "insecure-registries": ["127.0.0.1:5005", "192.168.1.50:5005"] }
  ```

  then `sudo systemctl restart docker` (or equivalent),
- for `SWARM_DEPLOY_ENABLED`: the docker socket it mounts must be the Swarm
  manager's (or `DOCKER_HOST` must point at it).

**Why not GitHub-hosted for these jobs?** The registry is on the fleet LAN
(loopback), so the push must originate on a host that can reach it.

## Enablement gates

Repository **Actions variables** (all default off — no runner, no registry,
and no stack change are required for a normal default-branch push):

| Variable | Default | Effect |
| --- | --- | --- |
| `FLEET_REGISTRY_PUSH_ENABLED` | off | `true` runs the fleet-registry push job |
| `FLEET_REGISTRY` | `127.0.0.1:5005` | Push target host |
| `DEPLOY_REGISTRY` | = `FLEET_REGISTRY` | Pull host the swarm nodes use |
| `SWARM_DEPLOY_ENABLED` | off | `true` (with push enabled) updates the stack |

Set them in **Settings → Secrets and variables → Actions → Variables**.

### Loopback vs LAN registry (when to set `DEPLOY_REGISTRY`)

- Runner and all swarm nodes on the **same host** (single-node swarm):
  `FLEET_REGISTRY=127.0.0.1:5005` and leave `DEPLOY_REGISTRY` unset.
- Swarm nodes pull over the network: keep the push on loopback if the runner
  is on the registry host, but set
  `DEPLOY_REGISTRY=192.168.1.50:5005` (the registry's LAN IP) so every node
  resolves the same registry. `127.0.0.1` in the stack would mean "this
  node's loopback" on each swarm node — broken for multi-node pulls.
- The stack pins the router to the manager (`node.role == manager`) because
  `fleet.yaml` is bind-mounted there.

## Bootstrap (first deploy)

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

6. **Enable the gates** in Actions variables: first
   `FLEET_REGISTRY_PUSH_ENABLED=true` (verify the image appears on `:5005`
   and the stack is unchanged), then `SWARM_DEPLOY_ENABLED=true`.

## CI update flow (after bootstrap)

```bash
# every main push with gates on
push:  self-hosted job bakes router -> :5005/ollama-router:{latest, sha-<7>}
deploy: self-hosted job runs docker stack deploy with ROUTER_TAG=sha-<7>
```

`docker stack deploy` updates only changed services; the update config uses
`order: stop-first` (never two replicas sharing FleetState) and
`failure_action: rollback`.

## Rollback

Point the stack back at a previous SHA tag from the local registry:

```bash
# turn the deploy gate off first (Settings -> Actions variables), then:
ROUTER_TAG=sha-<previous> DEPLOY_REGISTRY="${DEPLOY_REGISTRY:-127.0.0.1:5005}" \
  docker stack deploy -c deploy/swarm/ollama-router.stack.yml ollama-router
curl -fsS http://127.0.0.1:11434/healthz
```

The previous tag is still on the local registry (`latest` moved, `sha-*`
tags are immutable), so no rebuild is needed. Re-enable
`SWARM_DEPLOY_ENABLED` once verified. Disabling the gate does not touch the
running stack.

## Security notes

- No live tokens, admin bearer, zrok share tokens, or cloud keys in this
  directory or in git. Secrets are Swarm secrets or host env at deploy time.
- The admin API is fail-closed: with no token the router returns 403 on
  `/router/v1/*`.
- `deploy/swarm/` is scanned by CI for `ghcr.io` references — keep it
  local-registry only.
