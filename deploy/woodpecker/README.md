# Woodpecker CI — server + agent

Woodpecker CI replaces GitHub Actions for all ollama-router CI/CD: Rust
verify, Docker bake + `/healthz`, GHCR publish + provenance attestation,
fleet-registry push, swarm deploy, and the Pages site publish. Everything
runs on the fleet-hosted Woodpecker agent (Docker backend); no GitHub-hosted
runners are used. This ends the dependence on GitHub-hosted minutes and
unblocks CI regardless of GitHub account billing state.

## Components

- `compose.yaml` — `woodpeckerci/woodpecker-server:v3` (web UI + GitHub
  forge integration on loopback `:8000`, gRPC `:9000` on the Tailscale
  address) and a **CI** agent (`role=ci`, Docker socket on this host).
- `agent-nas.compose.yaml` — second agent on the swarm **manager**
  (`role=swarm-manager`, host network) so `docker stack deploy` can run.
  Pin `swarm-deploy.yml` to that label. Swarm overlay cannot reach the
  server gRPC (no MagicDNS on overlay; NAS Tailscale is containerized).
- `agent-nas.stack.yml` — overlay form; kept for a manager that has
  Tailscale in the host netns. Prefer the compose file.
- `.env.example` — copy to `.env` and fill in (never commit).

## Provisioning (first run)

1. **GitHub OAuth App** (owner account): Settings → Developer settings →
   OAuth Apps → New OAuth App.
   - Homepage URL: your `WOODPECKER_HOST` (e.g. `http://192.168.1.50:8000`)
   - Authorization callback URL: `${WOODPECKER_HOST}/authorize`
   - Copy the Client ID and Client secret into `.env`
     (`WOODPECKER_GITHUB_CLIENT` / `WOODPECKER_GITHUB_SECRET`).

2. **Agent secret**: `openssl rand -hex 32` → `WOODPECKER_AGENT_SECRET`
   (shared between server and agent; never commit).

3. **Start**:
   ```bash
   cd deploy/woodpecker
   cp .env.example .env      # fill in WOODPECKER_*
   docker compose up -d
   ```

4. **Activate the repository** (admin rights required): open
   `http://127.0.0.1:8000/` (compose binds loopback only), log in via GitHub, add a new repository
   and activate `miguelenes/ollama-router`. GitHub webhooks cannot reach loopback;
   trigger `verify.yml` from the UI (`manual`) after activate until a public
   `WOODPECKER_HOST` exists. Woodpecker still registers the forge webhook.

5. **Mark the repository as trusted** (Repo Settings → trusted). The agent
   pipelines mount `/var/run/docker.sock` and run `docker stack deploy`;
   volume mounts require a trusted repository. The agent is dedicated to
   this fleet — keep fork pull requests disabled so untrusted code never
   runs on a Docker-socket host. Pipelines listen to `push` (and `manual` for
   verify) so fork PRs — `pull_request` on this repo — never schedule. Do not
   approve blocked fork pipelines in the UI.

6. **Confirm the agent is online** (server UI → Agents shows
   `woodpecker-agent`). For swarm deploy, also run
   `agent-nas.compose.yaml` on the manager (host network, manager Docker
   socket) after publishing gRPC `:9000` on the LAN address the NAS can
   reach (`WOODPECKER_GRPC_LAN_BIND`, default `192.168.100.135`).
   `swarm-deploy.yml` is labeled `role=swarm-manager`; all other pipelines
   are `role=ci`.

## Repository secrets

Create them with `woodpecker-cli` (or the server UI → Repository →
Secrets). `woodpecker-cli` needs a token from the server UI (profile →
token); set `WOODPECKER_SERVER` and `WOODPECKER_TOKEN` env vars.

```bash
# GHCR publish + visibility check (classic PAT — fine-grained tokens do not
# support Packages; scope write:packages, optionally read:packages)
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name REGISTRY_TOKEN --value <classic-pat> \
  --event push --event tag --event manual

# Pages: push site/dist to the gh-pages branch (fine-grained PAT,
# contents:write, this repo only)
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name PAGES_TOKEN --value <token> \
  --event push --event manual

# Optional registry overrides (absent = defaults)
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name FLEET_REGISTRY --value 127.0.0.1:5005 --event push
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name FLEET_REGISTRY_REPLICA --value 192.168.100.5:5005 --event push
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name DEPLOY_REGISTRY --value 127.0.0.1:5005 --event push
```

**Enablement gates** (all default OFF — leave these secrets absent):

```bash
# Turn the fleet-registry push pipeline on:
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name FLEET_REGISTRY_PUSH_ENABLED --value true --event push
# Turn the swarm deploy pipeline on (requires the push gate too):
woodpecker-cli secret add --repository miguelenes/ollama-router \
  --name SWARM_DEPLOY_ENABLED --value true --event push

# Disable again:
# woodpecker-cli secret rm --repository miguelenes/ollama-router --name FLEET_REGISTRY_PUSH_ENABLED
```

With a gate secret absent or not `true`, the gated pipeline steps are
skipped (the verify pipeline still passes) — same semantics as the old
Actions variables.

## Pipelines

Each file in `.woodpecker/` is an independent workflow (see repo root):

| File | Runs on | Purpose |
| --- | --- | --- |
| `.woodpecker/verify.yml` | push (same-repo branches), manual | fmt, clippy, test, deny, coverage ≥80%, no-`ghcr.io` stack-refs grep |
| `.woodpecker/image.yml` | push (same-repo branches) | bake `router`, run container, probe `/healthz` |
| `.woodpecker/publish-ghcr.yml` | push (main), tag `v*` | push `router` to `ghcr.io/<owner>/<repo>` (edge/sha/semver/latest) + provenance attestation when public |
| `.woodpecker/fleet-push.yml` | push (main), gate | push `router` to fleet registry (+ optional `FLEET_REGISTRY_REPLICA`) |
| `.woodpecker/swarm-deploy.yml` | push (main), both gates | `docker stack deploy` the new SHA tag, verify image tag + `/healthz` (depends on fleet push) |
| `.woodpecker/pages.yml` | push (main), manual | build the Starlight site, push `site/dist` to `gh-pages` |

## Agent host requirements

- **Docker socket**: the agent mounts `/var/run/docker.sock`; the pipeline
  steps it starts run containers on this daemon.
- **Fleet registry trust**: the daemon must trust the local registry as an
  insecure registry (the push host reaches `:5005`, and the replica LAN host
  when `FLEET_REGISTRY_REPLICA` is set):
  ```json
  {
    "insecure-registries": [
      "127.0.0.1:5005",
      "192.168.100.135:5005",
      "192.168.100.5:5005"
    ]
  }
  ```
  then restart Docker. Loopback vs LAN (`DEPLOY_REGISTRY`, `FLEET_REGISTRY_REPLICA`)
  guidance lives in `deploy/swarm/README.md`.
- **Swarm manager access**: for `SWARM_DEPLOY_ENABLED`, run
  `agent-nas.compose.yaml` on the manager (host network; this compose agent
  is a worker and cannot `docker stack deploy`). The NAS agent reaches the
  server at `WOODPECKER_GRPC_LAN_BIND:9000` (default `192.168.100.135`)
  using the same `WOODPECKER_AGENT_SECRET`. Overlay (`agent-nas.stack.yml`)
  cannot resolve MagicDNS and the NAS host netns has no `tailscale0`.
- The agent replaces the old GitHub Actions self-hosted runner
  (`deploy/swarm/runner.compose.yml` is superseded and pending removal).

## Security notes

- No live tokens in this directory or in git: everything is a Woodpecker
  secret or `.env` (git-ignored). Never commit `.env`.
- The agent is root-equivalent via the Docker socket — do not run untrusted
  fork PRs on it. Workflows omit `pull_request` so fork events never start;
  the server also pins `WOODPECKER_DEFAULT_APPROVAL_MODE=forks`.
- Logs: never echo `REGISTRY_TOKEN` / `PAGES_TOKEN` / gate values;
  Woodpecker masks secret values in pipeline logs.
