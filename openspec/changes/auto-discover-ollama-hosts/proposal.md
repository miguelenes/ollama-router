## Why

Operators should not have to copy Ollama LAN or Tailscale IPs into `fleet.yaml` for the router to see them. Today the Swarm inventory is a hand-maintained address list (`desktop-pc`, `nuc`, `illuma`, … on `:11435`); hosts that move, get a new DHCP lease, or join the tailnet stay invisible until someone edits GitOps. The honest-fleet proxy contract (list = union, infer = holders, pull = placement, miss = 503, 501 mutates) stays.

## What Changes

- The router **finds** on-LAN / on-tailnet Ollama hosts by itself: a bounded CIDR scan plus Tailscale **peer enumeration** (not a brute-force scan of `100.64.0.0/10`), together with the existing optional node-agent enroll heartbeat extended so **LAN agents can call in without zrok share tokens**.
- `fleet.yaml` becomes an **optional pin**. Empty or absent inventory is valid when discovery is enabled; listed nodes still pin id / URL / labels. Discovery and enroll **MUST NOT** write `fleet.yaml`.
- Discovered members are a new origin `discovered` (not Verda/RunPod, not `fleet.yaml` `permanent`). They never count toward cloud envelopes and are **never destroyed** by idle/orphan teardown.
- Direct HTTP is allowed for RFC1918 and Tailscale CGNAT (`100.64.0.0/10`) on `permanent` / `discovered` / `adopt` rows. Cloud origins stay tunnel/loopback-only; public `:11434`, `*.zrok.io`, and globally routable IPs stay `public_url_blocked`.
- Scan adopts a host only when **node-agent `:11436` answers** (Ollama-only listeners are ignored). Heartbeat remains fail-closed admin bearer. Random laptops without the agent (or without the bearer, for heartbeat) do not join.
- Default remains **off** until tunables set `discovery.enabled` and at least one CIDR and/or Tailscale peer enum. Swarm overlay ships example CIDRs (`192.168.100.0/24`) and Tailscale enum on.

Not in this change: mDNS/Bonjour, Kubernetes/Swarm DNS of an `ollama` service, the router joining Tailscale, scanning `0.0.0.0/0` or the whole `100.64.0.0/10`, writing `fleet.yaml`, destroying discovered or `fleet.yaml` hosts, public tunnels as healthy, Thunder, native Hub-pull through one node, default `auto_pull_on_miss`, 404-on-miss, agent-down → unhealthy, Redis/HA replicas, ranking/placement algorithm changes.

## Capabilities

### New Capabilities

- `host-discovery`: Opt-in LAN/Tailscale discovery of node-agent hosts, identity from hostname, collision with `fleet.yaml` pins, forget TTL, self-exclusion of the router listen URL, and operator-visible discovery metrics (no model-name labels).

### Modified Capabilities

- `admin-nodes`: `origin=discovered` in listing / readiness / `node_info`; LAN enroll without share tokens; readiness counts include `discovered`.
- `node-agent`: Optional heartbeat can POST LAN `ollama_url` / `capacity_url` (RFC1918 or Tailscale CGNAT) without zrok share ids; still never logs tokens or bodies.
- `cloud-autoscale`: Idle/orphan teardown never destroys `discovered` hosts (same as `fleet.yaml` / `adopt`).
- `swarmnet-local-deploy`: Swarm inventory MAY be empty when discovery is enabled; CI still never writes `fleet.yaml`; example tunables list CIDRs + Tailscale enum.
- `public-docs-site`: Fleet guide documents optional `fleet.yaml` pins plus discovery CIDRs / Tailscale enum / agent heartbeat.

## Impact

- `crates/ollama-router-core/src/config/` — `discovery:` tunables + knobs; still reject overlay `nodes:`.
- `crates/ollama-router-core/src/fleet/` — `NodeOrigin::Discovered`; URL policy allows Tailscale CGNAT for non-cloud origins; registry upsert/forget; never write `fleet.yaml`.
- `crates/ollama-router-core/src/cloud/` — idle/orphan candidates exclude `discovered`.
- `crates/ollama-router/` — scan loop (Tokio, no blocking I/O in handlers), Tailscale LocalAPI (soft-fail), enroll LAN path, metrics, OpenAPI, console origin label.
- `crates/ollama-node-agent/` — LAN heartbeat payload when shares are absent.
- `deploy/swarm/` — example config CIDRs + Tailscale socket mount; `fleet.swarm.yaml` may shrink to pins or empty `nodes:`.
- Tests: httpmock scan/heartbeat; Tailscale enum fixture; origin/readiness; URL policy CGNAT for discovered vs blocked for cloud; `task check` + coverage.
- Wiki (`ollama-router-product.md`, `ollama-capacity-discovery.md`) + fleet-invariants rule + site fleet guide.
- Sensitivity: membership probes hit agent `/v1/status` and Ollama `/api/tags` or `/api/version` only. `/healthz` and `/readyz` are for skipping this router process, not for adopting a host. Never log bodies, agent tokens, admin bearer, or share ids.
