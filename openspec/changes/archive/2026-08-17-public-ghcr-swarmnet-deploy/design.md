## Context

See `proposal.md` (Why / What Changes). In-tree today: Astro Starlight `site/`, Apache-2.0 `LICENSE`, bake target `router`, `deploy/swarm/` stack compose + bootstrap/rollback runbook, README already linking the public Pages URL. The repository is already **public**. All CI/CD currently lives in `.github/workflows/` (ci, docker, pages, deploy-swarm, codeql, dependency-review, release-agent) on GitHub-hosted runners, which are locked out by a GitHub account billing issue; no GitHub-hosted job has started for days. There is no Woodpecker anywhere in the repo. Swarmnet already runs a fleet local registry (`127.0.0.1:5005` / LAN `:5005`) and its own stacks; ollama-router has no fleet-hosted CI yet.

Honest-fleet proxy behavior is out of scope for this design.

## Goals / Non-Goals

**Goals**

- Run **all** CI/CD on a self-hosted Woodpecker fleet agent (Docker backend), ending reliance on GitHub-hosted minutes and unblocking CI despite the billing lock.
- Keep the two-registry split: public packages (GHCR) vs fleet deploy (local registry only).
- Ship Woodpecker provisioning, pipelines, and the existing Swarm stack + runbook, without teaching swarmnet's control plane to manage this repo (optional later).
- Remove GitHub Actions workflows once Woodpecker is green.

**Non-Goals (design-level)**

- Dual-tagging the swarm deploy digest to GHCR.
- Auth/pull-through from GHCR on swarm nodes for this service.
- Keeping CodeQL or dependency-review (GitHub-Actions-only features).
- Moving Rust verification to any non-fleet runner.
- Adopting ollama-router as a swarmnet catalog entry in this change.
- Replacing `.github/workflows/release-agent.yml` with a Woodpecker pipeline (agent packages stay `task agent:release` until a follow-up).

## Decisions

1. **Woodpecker server + agent are provisioned in-repo (`deploy/woodpecker/`)**  
   Compose runs `woodpeckerci/woodpecker-server:v3` (persistent volume, `WOODPECKER_HOST`, GitHub OAuth App client/secret, `WOODPECKER_OPEN`) and `woodpeckerci/woodpecker-agent:v3` mounting `/var/run/docker.sock`, registered with `WOODPECKER_AGENT_SECRET`. The repo is activated from the server UI (admin rights), which auto-registers the webhook (push, pull_request).  
   **Rejected:** reusing a hypothetical external CI; there is none. A non-Docker backend (local/kubernetes) adds no value here.

2. **One pipeline file per concern in `.woodpecker/`**  
   Each file in the `.woodpecker/` folder is an independent workflow: `verify.yml`, `image.yml`, `publish-ghcr.yml`, `fleet-push.yml`, `swarm-deploy.yml`, `pages.yml`. `when:` filters select events (`push` on `main`, `tag` `v*`, `pull_request`, `manual`); `swarm-deploy` declares `depends_on: [fleet-push]`. Fork pull requests are not executed on the fleet agent (untrusted code on a Docker-socket host).  
   **Rejected:** one monolith pipeline with `depends_on` stages — harder to gate and to skip per concern.

3. **Two registries, two push pipelines**  
   `publish-ghcr.yml` pushes the bake `router` target to `ghcr.io/<owner>/<repo>` (edge/sha/semver/latest) for public consumers. `fleet-push.yml` pushes **only** to `FLEET_REGISTRY` (default `127.0.0.1:5005`) tagged `latest` + SHA.  
   **Rejected:** a single pipeline pushing both registries (violates "swarm deploy only local registry" and couples public CDN outages to fleet deploys).

4. **Enablement gates and registry hostnames are Woodpecker repo secrets**  
   `FLEET_REGISTRY_PUSH_ENABLED` and `SWARM_DEPLOY_ENABLED` are repository secrets; absent = off. A guard step in each gated pipeline reads the secret and exits 0 (skip) unless it is `true`; `swarm-deploy` additionally requires `FLEET_REGISTRY_PUSH_ENABLED == 'true'` (deploy is separately gated from push). Registry hostnames (`FLEET_REGISTRY`, optional `DEPLOY_REGISTRY`) are also repository secrets (Woodpecker has no Actions-style variables) so the stack can interpolate the pull host without committing it.  
   **Rejected:** checking in literal values (never — secrets and hostnames stay out of git).

5. **GHCR auth + provenance attestation from the agent**  
   The GitHub OAuth App token does not carry `packages:write`; a classic PAT with `write:packages` (optionally `read:packages`) is stored as the Woodpecker repo secret `REGISTRY_TOKEN` and used for `docker login ghcr.io`. Attestation uses `docker buildx build --attest=type=provenance,mode=max --push` (in-registry provenance manifests). Because Woodpecker has no `github.event.repository.private`, the pipeline resolves repo visibility via the GitHub API using the forge token (or an owner-set `REPO_IS_PUBLIC` secret) and runs the attestation only when the repository is public.  
   **Rejected:** `actions/attest-build-provenance` (GitHub-Actions-only).

6. **Pages deploys from a branch, not Actions**  
   GitHub Pages source becomes **Deploy from a branch** (`gh-pages`). The `pages.yml` Woodpecker pipeline builds the site (`npm ci`, `npm run lint:openapi`, `npm run build:openapi`, `npm run build`, secret scan over `src`/`public`/`openapi`) and pushes `site/dist` to `gh-pages` with a `PAGES_TOKEN` (`contents:write`).  
   **Rejected:** keeping Pages source = GitHub Actions (impossible without Actions runs; the billing lock would keep the site down).

7. **Deploy mechanism: `docker stack deploy` on the agent**  
   The swarm-deploy pipeline runs `docker stack deploy --prune --with-registry-auth -c deploy/swarm/ollama-router.stack.yml ollama-router` against the Swarm control plane reachable from the agent (Docker socket), interpolating `${DEPLOY_REGISTRY}` and `${ROUTER_TAG}`. CI never writes `fleet.yaml`.  
   **Rejected:** Portainer API (no long-lived cloud credentials on the agent; the socket is already there).

8. **GitHub Actions is removed after Woodpecker is green**  
   `.github/workflows/{ci,docker,pages,deploy-swarm,codeql,dependency-review,release-agent}.yml` are deleted in one commit once the Woodpecker pipelines pass. CodeQL (code scanning) and dependency-review are GitHub-Actions-only and are dropped; the site secret scan moves into the `pages.yml` pipeline. `release-agent.yml` is removed without a Woodpecker counterpart — agent packages stay `task agent:release` until a follow-up.  
   **Rejected:** keeping Actions in parallel (double CI drift); deleting before Woodpecker is proven (no rollback cushion).

9. **Stack shape is unchanged**  
   `deploy/swarm/ollama-router.stack.yml` (one `router` replica, `${DEPLOY_REGISTRY:-127.0.0.1:5005}/ollama-router:${ROUTER_TAG:-latest}`, fleet.yaml read-only mount, secret/env placeholders) and `deploy/swarm/README.md` bootstrap + rollback are reused as-is; only secret provisioning wording moves to Woodpecker secrets.

## Risks / Trade-offs

- **[Risk]** Fleet agent offline → all pipelines stuck (verify too) → **Mitigation:** gates default off for deploy; runbook documents agent health; `docker stack deploy` rollback uses the previous SHA tag from the local registry.
- **[Risk]** Docker socket exposure on the agent is root-equivalent → **Mitigation:** dedicated agent host; fork PRs not executed; `WOODPECKER_AGENT_SECRET`; registry tokens and deploy gates are repo secrets, never in git.
- **[Risk]** GHCR PAT scope creep / rotation → **Mitigation:** classic PAT scoped to `write:packages` (optionally `read:packages`); Pages uses its own dedicated fine-grained `PAGES_TOKEN` (`contents:write`, this repo only); rotation documented in `deploy/woodpecker/README.md`.
- **[Risk]** GitHub OAuth App token limits (webhook push coverage, rate limits) → **Mitigation:** App token used only for forge hooks; GHCR/Pages use scoped tokens.
- **[Risk]** Attestation gated on visibility mis-detected → **Mitigation:** pipeline reads visibility from the GitHub API; owner-set `REPO_IS_PUBLIC` override documented; attestation failure does not fail the publish when the repo is private.
- **[Risk]** Transition double-CI drift (Actions + Woodpecker both pushing) → **Mitigation:** gates absent during transition so fleet push/deploy are inert; Actions removed in one commit after Woodpecker is green.
- **[Risk]** Loss of GitHub code scanning (CodeQL) → **Mitigation:** accepted tradeoff; secret scan runs in `pages.yml`/verify; security review remains in PRs.

## Migration Plan

1. Provision `deploy/woodpecker/` (server + agent), create the GitHub OAuth App, set `WOODPECKER_HOST`, start the stack, activate the repository, confirm the agent is online.
2. Land `.woodpecker/` pipelines with gates **unset** (off); keep `.github/workflows/` until Woodpecker is proven.
3. Confirm verify, image/healthz, GHCR publish + attestation, and Pages branch deploy on Woodpecker.
4. Set Pages source = Deploy from a branch (`gh-pages`); confirm the public site URL serves content.
5. Remove `.github/workflows/` in one commit.
6. Enable `FLEET_REGISTRY_PUSH_ENABLED`; confirm the image on `:5005`; then enable `SWARM_DEPLOY_ENABLED`; confirm the stack updates and `/healthz` responds.
7. Owner confirms GHCR package visibility + attestation on the next publish and the anonymous pull.
8. Rollback: revert the Actions-removal commit, disable the deploy gate, `docker stack deploy` the previous SHA tag from the local registry; visibility stays public.

## Open Questions

- Which host runs the Woodpecker agent (a swarmnet node vs a dedicated host) — affects only the runbook and `DEPLOY_REGISTRY` value, not the pipelines.

### Resolved during planning

- Pages is driven by a dedicated fine-grained `PAGES_TOKEN` (`contents:write`, this repo only), not the GitHub OAuth App token (see `deploy/woodpecker/README.md`).
- Pages branch is `gh-pages` (Deploy from a branch), per the `public-docs-site` spec and task 5.3.
- GHCR `latest` tag semantics are unchanged: edge/sha on default-branch pushes, semver + `latest` on `v*` tags, per the `ghcr-package-publish` spec and Decision 3.
