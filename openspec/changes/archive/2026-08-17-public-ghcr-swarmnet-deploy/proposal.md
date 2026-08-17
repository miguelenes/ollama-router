## Why

The go-public docs site, Apache-2.0 license, and GHCR publish path already exist in-tree, but CI still runs on GitHub-hosted runners — which are currently locked out by a GitHub account billing issue — and there is no fleet-hosted CI or Swarm stack path to run the router on the home fleet (swarmnet). Moving CI/CD to Woodpecker CI on fleet agents makes the project publicly usable and operable entirely on home-fleet infrastructure, ending dependence on GitHub-hosted minutes and GitHub Actions.

## What Changes

- **CI/CD moves to Woodpecker CI.** Provision a Woodpecker server + agent in `deploy/` and replace all `.github/workflows/*` with `.woodpecker/*` pipelines: Rust verify (fmt/clippy/test/deny/coverage), Docker bake + `/healthz`, GHCR publish + provenance attestation, fleet-registry push, swarm deploy, and Pages. **BREAKING**: GitHub-hosted runners and GitHub Actions-based CI are no longer used; CodeQL and dependency-review, which exist only as GitHub Actions, are removed.
- **Owner gate:** repository visibility is already public; the remaining owner steps are GitHub settings actions: configure Pages source = Deploy from a branch (`gh-pages`), confirm a green Pages deploy, and confirm GHCR package visibility and attestation on the next publish.
- **GitHub Pages:** Pages serves `https://<owner>.github.io/ollama-router/` from the `gh-pages` branch published by the Woodpecker Pages pipeline (Actions source is removed because it requires Actions runs). The README already links the public site URL.
- **GHCR packages:** keep publishing the `router` image to `ghcr.io/<owner>/ollama-router` from the Woodpecker agent (repository packages for public consumers), with build provenance attestation when the repository is public.
- **Fleet CI:** all jobs run on a fleet-hosted Woodpecker agent (Docker backend) that can reach the local registry and the Swarm control plane; no GitHub-hosted runners.
- **Swarmnet deploy:** a gated push-on-merge path bakes `router` and pushes **only** to the **fleet local registry** (e.g. `127.0.0.1:5005` / LAN registry host), then updates/deploys the ollama-router Swarm stack. Swarm deploy MUST NOT pull or push GHCR on that path.
- Keeps the **honest-fleet contract** unchanged (union list, holders-only infer, placement pull, 503 `model_missing`, 501 mutates, default-false `auto_pull_on_miss`).

### Non-goals

- Thunder / any third cloud provider
- Dual-publish of the swarm deploy image to GHCR (swarm path = local registry only)
- Pulling swarm stack images from GHCR on swarm nodes
- Changing proxy routing, idle destroy, enroll, or fleet.yaml inventory rules
- Native Hub-pull through one node, default `auto_pull_on_miss`, 404-on-miss, public tunnels healthy
- HA / multi-replica router, Redis, or writing fleet.yaml from CI
- Replacing or rewriting the Starlight site content (already shipped)
- Keeping GitHub Actions or GitHub-hosted runners anywhere in the CI path
- Replacing node-agent package CI (`.github/workflows/release-agent.yml`); that workflow is removed with GitHub Actions and is not rebuilt in Woodpecker in this change (`task agent:release` remains the local path until a follow-up)

## Capabilities

### New Capabilities

- `ghcr-package-publish`: public Container Registry publish of the router image to GHCR with tags suitable for consumers; attestations when the repo is public.
- `ci-local-runner`: fleet-hosted CI execution — all pipelines (verify, bake, publish, deploy, Pages) run on a Woodpecker agent on the home fleet, with clear enablement gates.
- `swarmnet-local-deploy`: deploy/update the ollama-router stack on the swarmnet Docker Swarm using images from the fleet local registry only (not GHCR).

### Modified Capabilities

- `public-docs-site`: Pages publish moves from a GitHub Actions source to a Woodpecker-published `gh-pages` branch; the remaining owner gate is Pages source = Deploy from a branch, not a visibility flip (the repository is already public).

## Impact

- `.github/workflows/` — removed after Woodpecker is green (ci, docker, pages, deploy-swarm, codeql, dependency-review, release-agent).
- `.woodpecker/` — new pipeline files (verify, image, publish-ghcr, fleet-push, swarm-deploy, pages).
- `deploy/` — Woodpecker server + agent compose and activation runbook; existing Swarm stack compose + runbook reused unchanged.
- `docker-bake.hcl` / Dockerfile — unchanged; the bake `router` target is used by all push pipelines.
- README — public site URL / package install pointers already in place.
- No Rust crate behavior changes; no proxy API contract changes. Sensitivity: pipelines and runbooks must not embed admin tokens, registry credentials beyond Woodpecker secrets, or live prompts.
