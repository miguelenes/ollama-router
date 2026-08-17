## 1. Document registry topology (no merge)

- [ ] 1.1 Add an additive **Registry roles (push vs Hub cache)** section to `deploy/swarm/README.md` **after `## Architecture` and before `## Fleet CI agent (Woodpecker)`** — distinct from **Two registries, two jobs** (GHCR vs fleet deploy); do not rewrite push/replica/buildx content from fleet-registry-push-fix. Cover: fleet push registry (`:5005`, push required), optional `FLEET_REGISTRY_REPLICA` as a second push-capable host (not the Hub cache), Hub pull-through cache (separate process); cite Distribution push-not-supported on proxy mode; document **`registry.bicorn-beta.ts.net`** (fleet push registry Tailscale, optional) vs **`cache.bicorn-beta.ts.net`** / NAS `:9999` (Hub mirror only); example daemon `registry-mirrors`; cross-link the existing split-agent secret table; optionally reference the NAS Portainer four-container pattern as out-of-repo fleet infra
- [ ] 1.2 Update `deploy/swarm/fleet-registry.compose.yaml` header comments: push-only filesystem registry for the CI agent host; do not add `REGISTRY_PROXY_REMOTEURL`; note NAS manager may use Portainer `registry` instead; point to README **Registry roles (push vs Hub cache)** for Hub cache and Tailscale hostnames

## 2. Validation

- [ ] 2.1 Confirm `deploy/swarm/fleet-registry.compose.yaml` has no proxy env vars and matches the push-capable spec scenarios
- [ ] 2.2 Sanity-check that existing fleet-push + `FLEET_REGISTRY_REPLICA` flow is unchanged (docs-only change)
