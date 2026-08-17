## Why

The go-public docs site, Apache-2.0 license, and GHCR workflow already exist in-tree, but the repository is still private, Pages is not promoted as the live public surface, CI still runs entirely on GitHub-hosted runners, and there is no path to run the router as a Swarm stack on the home fleet (swarmnet) from this repo. Closing that gap makes the project publicly usable and operable on the same local runner + local-registry pattern the fleet already uses.

## What Changes

- **Owner gate:** flip `miguelenes/ollama-router` from private to public once readiness checks pass (LICENSE already Apache-2.0; site, CONTRIBUTING, CoC, templates already present). Visibility flip is a GitHub settings action, not application code.
- **GitHub Pages:** ensure Pages source = Actions, the existing `pages.yml` deploys on `main`, and the public site URL is linked from the README (no README rewrite beyond the link / badge if needed).
- **GHCR packages:** keep publishing the `router` image to `ghcr.io/<owner>/ollama-router` from CI (repository packages for public consumers). Artifact attestations stay gated on `!repository.private` so they activate automatically after the visibility flip.
- **Local runner:** run **fleet-registry push** and **swarm deploy** on a self-hosted runner labeled for this fleet (same class as swarmnet’s `self-hosted,linux,…` pattern). GHCR publish may remain on `ubuntu-latest` (GitHub-hosted `GITHUB_TOKEN`). Verify/lint/test stay on `ubuntu-latest` unless the runner can absorb them without starving the fleet.
- **Swarmnet deploy:** add a gated push-on-merge path that builds the router image and pushes **only** to the **fleet local registry** (e.g. `127.0.0.1:5005` / LAN registry host), then updates/deploys the ollama-router Swarm stack. Swarm deploy MUST NOT pull or push GHCR for that path.
- Keeps the **honest-fleet contract** unchanged (union list, holders-only infer, placement pull, 503 `model_missing`, 501 mutates, default-false `auto_pull_on_miss`).

### Non-goals

- Thunder / any third cloud provider
- Dual-publish of the swarm deploy image to GHCR (swarm path = local registry only)
- Pulling swarm stack images from GHCR on swarm nodes
- Changing proxy routing, idle destroy, enroll, or fleet.yaml inventory rules
- Native Hub-pull through one node, default `auto_pull_on_miss`, 404-on-miss, public tunnels healthy
- HA / multi-replica router, Redis, or writing fleet.yaml from CI
- Replacing or rewriting the Starlight site content (already shipped)

## Capabilities

### New Capabilities

- `ghcr-package-publish`: public Container Registry publish of the router image to GHCR with tags suitable for consumers; attestations when the repo is public.
- `ci-local-runner`: self-hosted GitHub Actions runner usage for fleet-registry push and swarm deploy jobs, with clear labels and enablement gates (GHCR may stay GitHub-hosted).
- `swarmnet-local-deploy`: deploy/update the ollama-router stack on the swarmnet Docker Swarm using images from the fleet local registry only (not GHCR).

### Modified Capabilities

- `public-docs-site`: add a visibility-promotion / live-Pages requirement so going public includes enabling Pages Actions source and a discoverable public site URL (without changing content requirements already archived).

## Impact

- `.github/workflows/` — keep GHCR `docker.yml` (typically `ubuntu-latest`); add fleet-registry push + swarm deploy jobs on self-hosted `runs-on`, gated by `FLEET_REGISTRY_PUSH_ENABLED` and `SWARM_DEPLOY_ENABLED` (plus `FLEET_REGISTRY` / optional `DEPLOY_REGISTRY`).
- `docker-bake.hcl` / Dockerfile — tag/push wiring for local-registry target; GHCR path remains bake `router` target.
- `deploy/` — Swarm stack compose (or Portainer stack spec), runbook for first deploy and CI update against local registry; no secrets in git.
- README — public site URL / package install pointers only.
- No Rust crate behavior changes; no proxy API contract changes. Sensitivity: workflows and runbooks must not embed admin tokens, registry credentials beyond documented env/vars, or live prompts.
