## Why

The fleet runs two Distribution registry roles today: a **push-capable local registry** (`:5005`, fleet `ollama-router` tags) and a **Docker Hub pull-through cache** (`cache.bicorn-beta.ts.net` / NAS `:9999`, daemon `registry-mirrors`). Operators asked whether those can run as one container to reduce moving parts on the NAS and desktop CI host.

## What Changes

- **Answer the merge question in design**: Distribution proxy mode does **not** support push — a single pull-through cache instance cannot replace the fleet local registry that Woodpecker `fleet-push` writes to.
- Document the **two-role topology** in `deploy/swarm/README.md` under **Registry roles (push vs Hub cache)** — distinct from the existing **Two registries, two jobs** section (GHCR vs fleet deploy): fleet push/pull registry vs Hub mirror cache; when each is required; why they stay separate processes.
- Clarify in docs that **`FLEET_REGISTRY_REPLICA`** (split-agent push to the NAS manager registry LAN, e.g. `192.168.100.5:5005`) is a second **push-capable** registry — not a merge with the Hub pull-through cache on `:9999` / `cache.bicorn-beta.ts.net`.
- Keep `deploy/swarm/fleet-registry.compose.yaml` as a **single push-only** `registry:2` service (no proxy env). Do not fold Hub cache into that compose file.
- Optionally reference the NAS Portainer `registry` stack pattern (local `:5005` + cache `:9999` + Tailscale sidecars) as out-of-repo fleet infra — not merged into ollama-router's committed compose.

Follows archived **fleet-registry-push-fix** (buildx `--builder default`, `FLEET_REGISTRY_REPLICA`). This change is **docs-only** topology guidance — Woodpecker pipelines and replica secrets are unchanged.

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `swarmnet-local-deploy`: fleet local registry bootstrap SHALL remain push-capable and SHALL NOT be configured as a pull-through proxy that blocks `fleet-push`.

## Impact

- `deploy/swarm/README.md` — additive **Registry roles (push vs Hub cache)** section after Architecture (not a rewrite of **Two registries, two jobs** / push-replica/buildx docs from fleet-registry-push-fix).
- `deploy/swarm/fleet-registry.compose.yaml` — header comments only (push-only, no `REGISTRY_PROXY_REMOTEURL`; Distribution proxy docs reference).
- No Rust crates, Woodpecker pipeline behavior, or honest-fleet proxy changes.
- NAS Portainer `registry` stack (four containers today) is **documented**, not rewritten in this repo unless a follow-up exports it here.
