## Context

See `proposal.md` (Why / What Changes). In-tree today: Astro Starlight `site/`, `pages.yml`, Apache-2.0 `LICENSE`, GHCR `docker.yml` on `ubuntu-latest`, bake target `router`. Repo is still private. Swarmnet already runs a self-hosted Actions runner and a fleet local registry (`127.0.0.1:5005` / LAN `:5005`) for its own stacks; ollama-router has no Swarm stack or local-registry CI path yet.

Honest-fleet proxy behavior is out of scope for this design.

## Goals / Non-Goals

**Goals**

- Split **public packages** (GHCR) from **fleet deploy** (local registry only).
- Run heavy Docker push + swarm deploy on a self-hosted runner; keep verify cheap on GitHub-hosted unless the runner is proven enough.
- Ship a minimal one-replica Swarm stack + runbook + gated CI, without teaching swarmnet’s control plane to manage this repo (optional later).

**Non-Goals (design-level)**

- Dual-tagging the swarm deploy digest to GHCR.
- Auth/pull-through from GHCR on swarm nodes for this service.
- Adopting ollama-router as a swarmnet catalog entry in this change.
- Moving Rust CI off `ubuntu-latest` in the first cut (optional follow-up).

## Decisions

1. **Two registries, two jobs**  
   - Keep `docker.yml` → `ghcr.io/<owner>/ollama-router` for public consumers (may stay on `ubuntu-latest` with `GITHUB_TOKEN`, or move to self-hosted if the runner has reliable outbound + docker login).  
   - Add a separate workflow job (or `deploy-swarm.yml`) that bakes `router` and pushes **only** to `vars.FLEET_REGISTRY` (default `127.0.0.1:5005`), tagged `latest` + SHA.  
   - **Rejected:** single job that pushes both registries (violates “swarm deploy only local registry” and couples public CDN outages to fleet deploys).

2. **Self-hosted runner labels**  
   - Prefer labels `[self-hosted, linux, ollama-router]` on a runner that can reach the local registry (typically nas). Document reuse of the swarmnet runner host with an extra label, or a compose profile under `deploy/` for an ollama-router-scoped runner.  
   - **Rejected:** requiring all CI (Rust verify) on self-hosted in v1 — cold caches and queue contention with swarmnet builds.

3. **Enablement gates (Actions variables)**  
   - `FLEET_REGISTRY_PUSH_ENABLED` — push to local registry.  
   - `FLEET_REGISTRY` / optional `DEPLOY_REGISTRY` — push vs pull hostname split (same pattern as swarmnet).  
   - `SWARM_DEPLOY_ENABLED` — stack update after push.  
   - No repo secrets required for local insecure registry on loopback; admin/cloud secrets stay on the Swarm host as Docker secrets.

4. **Stack shape**  
   - Add `deploy/swarm/ollama-router.stack.yml` (or compose file suitable for `docker stack deploy` / Portainer): one `router` service, published or overlay-only listen as documented, mounts for `fleet.yaml` + FleetState volume, env/secrets for admin token and optional cloud keys.  
   - Image: `${FLEET_REGISTRY}/ollama-router:…` (or `127.0.0.1:5005/ollama-router`).  
   - **Rejected:** multi-replica (FleetState lock is same-host only).  
   - **Rejected:** CI rewriting `fleet.yaml`.

5. **Deploy mechanism**  
   - v1: self-hosted job runs `docker stack deploy` (or Portainer API if the runner already has those creds for swarmnet) against the stack file with the new tag. Prefer the simplest path the nas runner can do without new long-lived cloud credentials in GitHub.  
   - Document first-time bootstrap in `deploy/swarm/README.md` (create secrets, place fleet.yaml, enable variables).

6. **Public promotion checklist**  
   - Owner: Pages → Source = GitHub Actions; set visibility public; confirm GHCR package visibility; README link to `https://<owner>.github.io/ollama-router/`.  
   - Attestations in existing `docker.yml` already use `if: ${{ !github.event.repository.private }}`.

## Risks / Trade-offs

- **[Risk]** Self-hosted runner offline → deploy stuck while verify still green → **Mitigation:** gates default off; document runner health; do not block PR CI on fleet jobs.  
- **[Risk]** Local registry image not pullable from other swarm nodes if tagged `127.0.0.1` → **Mitigation:** use `DEPLOY_REGISTRY` / placement on the registry host, or LAN IP in stack refs as swarmnet does.  
- **[Risk]** Accidental GHCR pull in stack file → **Mitigation:** spec + review checklist; CI grep that stack image refs exclude `ghcr.io`.  
- **[Risk]** Public repo exposes workflow logs → **Mitigation:** existing sensitivity rules; never echo secrets; fleet registry has no public ingress requirement.

## Migration Plan

1. Land workflows + stack file with gates **off**.  
2. Register/label self-hosted runner; set `FLEET_REGISTRY`; enable push only; verify image on `:5005`.  
3. Bootstrap stack manually once; then enable `SWARM_DEPLOY_ENABLED`.  
4. Enable GitHub Pages Actions source; verify site URL.  
5. Owner flips repository to public; confirm GHCR + attestations + Pages.  
6. Rollback: disable deploy gate; `docker stack deploy` previous SHA tag from local registry; visibility can stay public.

## Open Questions

- Exact runner label string (`ollama-router` vs shared `swarmnet`) — choose at apply time based on whether one runner process can carry both labels without job theft.  
- Portainer API vs raw `docker stack deploy` — pick whichever the nas runner already authenticates for; does not change specs.
