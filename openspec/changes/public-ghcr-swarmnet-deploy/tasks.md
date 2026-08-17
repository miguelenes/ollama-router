## 1. GHCR package publish

- [x] 1.1 Confirm `.github/workflows/docker.yml` still bakes and pushes target `router` to `ghcr.io/<owner>/<repo>` with edge/sha/semver/`latest` metadata; keep attestation gated on `!github.event.repository.private` (spec: Router image is published to GHCR; Provenance attestation when the repository is public)
- [x] 1.2 Document in `deploy/swarm/README.md` (or a short `deploy/ghcr.md`) how consumers pull the public package vs the fleet local-registry image (no dual-push on the swarm path)

## 2. Self-hosted runner wiring

- [x] 2.1 Choose and document runner labels (`self-hosted`, `linux`, plus `ollama-router` and/or shared fleet label); add compose profile or runbook steps to register the runner on the host that can reach `:5005` (spec: Docker push and swarm deploy jobs use a self-hosted runner)
- [x] 2.2 Add fleet-registry push job(s) with `runs-on` set to those labels and gate `FLEET_REGISTRY_PUSH_ENABLED` (default off) (spec: Runner enablement is gated)

## 3. Fleet local registry push (swarm path)

- [x] 3.1 Add workflow steps that bake `router` and push **only** to `vars.FLEET_REGISTRY` (default `127.0.0.1:5005`) with `latest` + git SHA tags; do not push that job’s image to `ghcr.io` (spec: Swarm deploy images come only from the fleet local registry)
- [x] 3.2 Support optional `DEPLOY_REGISTRY` when pull hostname differs from push hostname; document nas loopback vs LAN IP (spec: Swarm deploy images come only from the fleet local registry)

## 4. Swarm stack and deploy gate

- [x] 4.1 Add `deploy/swarm/` stack compose (one router replica, local-registry image ref, mounts for fleet.yaml + FleetState, secret/env placeholders only) and a CI check or documented grep that stack refs exclude `ghcr.io` (spec: Stack image refs are local-registry only; One router replica and no fleet.yaml writes from CI)
- [x] 4.2 Add deploy job gated by `SWARM_DEPLOY_ENABLED` (in addition to push gate) that updates the stack via `docker stack deploy` or Portainer API—whichever the nas runner already supports—without writing `fleet.yaml` (spec: Deploy is separately gated from registry push)
- [x] 4.3 Write bootstrap + rollback runbook (`deploy/swarm/README.md`): create secrets, place inventory, enable variables, roll back to previous SHA tag (spec: Deploy artifacts contain no live secrets)

## 5. Public Pages and visibility

- [x] 5.1 Ensure README links the GitHub Pages URL (`/ollama-router/` base); verify `pages.yml` still builds and deploys on `main` (spec: README points at the live site; Pages Actions source is enabled)
- [ ] 5.2 OWNER GATE: configure Pages source = Actions, confirm a green Pages deploy, then set repository visibility to **public**; confirm GHCR package visibility and that attestations run on the next publish (spec: Visibility flip is an owner gate; Public promotion includes live GitHub Pages)

## 6. Validation

- [ ] 6.1 With gates off, confirm default-branch CI verify still passes without a self-hosted runner
- [ ] 6.2 With push gate on (deploy off), confirm local-registry image appears and stack is unchanged
- [ ] 6.3 With both gates on, confirm stack updates to the new SHA and `/healthz` responds on the published route
- [ ] 6.4 After public flip, confirm Pages URL serves the site and GHCR pull works anonymously (or per package visibility settings)
