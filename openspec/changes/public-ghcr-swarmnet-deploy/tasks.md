## 1. Woodpecker provisioning

- [x] 1.1 Add `deploy/woodpecker/` compose: `woodpeckerci/woodpecker-server:v3` + `woodpeckerci/woodpecker-agent:v3` (Docker socket mount, `WOODPECKER_AGENT_SECRET`), GitHub OAuth App env (`WOODPECKER_GITHUB_CLIENT`/`SECRET`, `WOODPECKER_HOST`, `WOODPECKER_OPEN`), persistent volumes, `.env.example`
- [x] 1.2 Write `deploy/woodpecker/README.md`: create GitHub OAuth App, set `WOODPECKER_HOST`, generate agent secret, start the stack, activate the repo (admin rights), confirm the agent is online
- [x] 1.3 Create Woodpecker repository secrets: `REGISTRY_TOKEN` (classic PAT, `write:packages`), `PAGES_TOKEN` (`contents:write` for `gh-pages`); leave gates `FLEET_REGISTRY_PUSH_ENABLED` / `SWARM_DEPLOY_ENABLED` unset (off)

## 2. Verify + image pipelines

- [x] 2.1 Add `.woodpecker/verify.yml`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked`, `cargo deny check advisories bans`, `cargo llvm-cov --fail-under-lines 80` (ignore `**/main.rs`); runs on the fleet agent (spec: Fleet-local jobs run on a fleet-hosted Woodpecker agent; Verify runs on the fleet agent)
- [x] 2.2 Add `.woodpecker/image.yml`: bake target `router`, run the container, probe `/healthz` on a non-conflicting published port (no port collision with a running router on the agent)
- [x] 2.3 Skip fork pull requests on the fleet agent so untrusted code never runs on the Docker-socket host (spec: Fork pull requests are not executed)

## 3. Publish pipelines

- [x] 3.1 Add `.woodpecker/publish-ghcr.yml`: push bake target `router` to `ghcr.io/<owner>/<repo>` (lowercase) with edge/sha/semver/`latest` metadata on default-branch push and `v*` tags; provenance attestation (`buildx --attest=type=provenance,mode=max`) when the repository is public (spec: Router image is published to GHCR; Provenance attestation when the repository is public)
- [x] 3.2 Add `.woodpecker/fleet-push.yml`: guard on `FLEET_REGISTRY_PUSH_ENABLED == 'true'`; bake `router` and push **only** to `FLEET_REGISTRY` (default `127.0.0.1:5005`) with `latest` + git SHA tags; never `ghcr.io` (spec: Swarm deploy images come only from the fleet local registry; Runner enablement is gated)
- [x] 3.3 Support optional `DEPLOY_REGISTRY` when the pull hostname differs from the push hostname; document nas loopback vs LAN IP (spec: Swarm deploy images come only from the fleet local registry)

## 4. Swarm deploy

- [x] 4.1 `deploy/swarm/ollama-router.stack.yml` already committed: one router replica, `${DEPLOY_REGISTRY:-127.0.0.1:5005}/ollama-router` image ref, fleet.yaml read-only mount, secret/env placeholders only — reused unchanged (spec: Stack image refs are local-registry only; One router replica and no fleet.yaml writes from CI)
- [x] 4.2 Move the no-`ghcr.io` stack-refs grep check into `.woodpecker/verify.yml` (stack image refs must exclude `ghcr.io`)
- [x] 4.3 Add `.woodpecker/swarm-deploy.yml`: `depends_on` fleet-push; guard on **both** `FLEET_REGISTRY_PUSH_ENABLED` and `SWARM_DEPLOY_ENABLED`; `docker stack deploy` the new SHA tag; never writes `fleet.yaml` (spec: Deploy is separately gated from registry push)
- [x] 4.4 Confirm `deploy/swarm/README.md` bootstrap + rollback (create secrets, place inventory, enable gates, roll back to previous SHA tag) matches Woodpecker secrets (spec: Deploy artifacts contain no live secrets)

## 5. Pages + visibility

- [x] 5.1 Add `.woodpecker/pages.yml`: build the site (`npm ci`, `npm run lint:openapi`, secret scan over `src`/`public`/`openapi`, `npm run build:openapi`, `npm run build`), push `site/dist` to the `gh-pages` branch (spec: Pages branch is published; Site is published to GitHub Pages; Pages publish runs on the fleet agent)
- [x] 5.2 README already links the public site URL (path base `/ollama-router/`) — committed (spec: README points at the live site)
- [x] 5.3 OWNER GATE: set Pages source = Deploy from a branch (`gh-pages`), confirm a green Pages deploy, confirm GHCR package visibility and that attestation runs on the next publish (spec: Pages source is an owner gate; Visibility flip is an owner gate; Public promotion includes live GitHub Pages)

## 6. Decommission GitHub Actions + validation

- [x] 6.1 Remove `.github/workflows/` (ci, docker, pages, deploy-swarm, codeql, dependency-review, release-agent) once the Woodpecker pipelines are green
- [x] 6.2 With gates unset, confirm default-branch verify still passes on the fleet agent (no GitHub-hosted runner)
- [x] 6.3 With push gate on (deploy off), confirm the local-registry image appears and the stack is unchanged
- [ ] 6.4 With both gates on, confirm the stack updates to the new SHA and `/healthz` responds on the published route
- [x] 6.5 Confirm the Pages URL serves the site and GHCR pull works anonymously (or per package visibility settings)
