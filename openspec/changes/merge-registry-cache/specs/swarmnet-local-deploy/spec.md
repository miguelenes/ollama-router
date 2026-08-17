## ADDED Requirements

### Requirement: Fleet local registry remains push-capable

The fleet local registry used for Woodpecker `fleet-push` and Swarm deploy pulls SHALL run as a standard Distribution filesystem registry and SHALL NOT be configured as a pull-through proxy (`proxy.remoteurl` / `REGISTRY_PROXY_REMOTEURL`) for an upstream such as Docker Hub. Hub image caching SHALL use a separate registry instance or an existing fleet mirror endpoint documented for operators.

#### Scenario: Fleet-push succeeds against the local registry

- **WHEN** the fleet local registry is running per `deploy/swarm/fleet-registry.compose.yaml` and Woodpecker fleet-push publishes `ollama-router:latest` and `ollama-router:sha-<git>`
- **THEN** both tags are accepted via `docker push` and are visible on the registry API at the configured push host

#### Scenario: Hub pull-through cache is not the fleet push target

- **WHEN** an operator inspects the committed fleet local registry bootstrap for swarm deploy
- **THEN** it does not enable Distribution pull-through proxy mode on the `:5005` fleet push registry

#### Scenario: Hub cache remains a separate role

- **WHEN** fleet hosts need accelerated Docker Hub pulls (for example via daemon `registry-mirrors`)
- **THEN** documentation describes the Hub cache as a separate registry process or fleet mirror hostname, not as a merged replacement for the push-capable fleet registry on `:5005`

#### Scenario: Replica registry is push-capable

- **WHEN** `FLEET_REGISTRY_REPLICA` is set for split-agent fleet-push
- **THEN** that host is a filesystem push registry (same constraints as the primary fleet registry) and is not configured as a pull-through proxy
