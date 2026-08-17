## 1. Fleet local registry compose

- [x] 1.1 Add `deploy/swarm/fleet-registry.compose.yaml` (`registry:2`, loopback `:5005`, documented LAN bind, persistent volume) matching the runbook reference

## 2. Fleet-push buildx fix

- [x] 2.1 Update `.woodpecker/fleet-push.yml`: pin `--builder default` on the build command, and extend the file header comments to explain loopback `FLEET_REGISTRY` vs container-isolated builders (`swarmos`)

## 3. Fleet-push manager registry replication

- [x] 3.1 Add optional `FLEET_REGISTRY_REPLICA` secret to `.woodpecker/fleet-push.yml`; when set, add replica `-t` tags (`latest` + `sha-<git>`) to the same buildx `--push`
- [x] 3.2 Document `FLEET_REGISTRY_REPLICA` in `deploy/swarm/README.md` and `deploy/woodpecker/README.md` (split-agent pattern: desktop push + NAS manager pull)

## 4. Runbook documentation

- [x] 4.1 Update `deploy/swarm/README.md`: verify bootstrap step zero matches the committed compose path and insecure-registry instructions; add buildx builder guidance for loopback push; document the three-secret split (`FLEET_REGISTRY`, `FLEET_REGISTRY_REPLICA`, `DEPLOY_REGISTRY`)

## 5. Validation

- [x] 5.1 With registry running on the CI agent, confirm fleet-push publishes `ollama-router:latest` and `ollama-router:sha-<git>` on the primary host (`127.0.0.1:5005`)
- [x] 5.2 With `FLEET_REGISTRY_REPLICA=192.168.100.5:5005`, confirm the same tags appear on the manager registry and swarm service can roll to the new SHA (validated manually on this fleet)
