## Context

See `proposal.md` (Why) and the delta specs under `specs/` for requirements. Current inventory is `fleet.yaml` (`load_fleet_nodes` already allows a missing default path → empty list) + FleetState + Verda/RunPod. Overlay YAML still rejects `nodes:`. `NodeOrigin` is `{Permanent, Adopt, Verda, Runpod}`. `upsert_adopt` does not overwrite permanent/cloud; `apply_permanent_inventory` only diffs `Permanent` rows.

Agent enroll heartbeat (`crates/ollama-node-agent/src/register.rs`) is off unless `register.url` is set, and **skips when zrok share ids are missing** — so LAN hosts cannot call in today. `EnrollRequest` requires `ollama_share_id` / `agent_share_id` strings (`deny_unknown_fields`).

`routing_url_blocked_reason` treats Tailscale CGNAT (`100.64.0.0/10`) as `public_url_blocked` for every origin. `hydrate_node_urls` therefore replaces fleet.yaml `100.x` URLs with FleetState tunnel URLs. Swarm `deploy/fleet.swarm.yaml` still lists those CGNAT hosts as direct HTTP on `:11435`.

Health already uses Tokio `Semaphore` + per-node `spawn` (`crates/ollama-router/src/health.rs`) and reqwest with capped bodies (`read_reqwest_capped`). Enroll is Axum 0.8 `Json<EnrollRequest>`. Context7 was quota-limited this session; the design reuses those in-tree 0.8 / Tokio / reqwest patterns rather than new GraphQL/WS APIs. Do not rewrite `rank_nodes` or the job orchestrator.

## Goals / Non-Goals

**Goals:**

- Opt-in discovery loop in the router binary that expands bounded CIDRs, enumerates Tailscale peers, probes node-agent then Ollama, and upserts `NodeOrigin::Discovered` without writing `fleet.yaml`.
- Origin-aware URL policy so RFC1918 and CGNAT are direct-HTTP for `permanent` / `discovered` / `adopt`, while cloud stays tunnel/loopback-only.
- LAN enroll path (scan + heartbeat) with optional share ids; fail-closed admin bearer unchanged.
- Forget = registry removal only; idle/orphan policy still matches `origin == provider` (Verda/Runpod) so discovered hosts cannot be cloud-destroyed.

**Non-Goals (design-level):**

- No `rank_nodes` / placement / job-orchestrator changes; newly discovered capacity stays omitted (unknown), not `vram_gb = 0` / `gpus = 0`.
- No new HTTP client crate, no `hyperlocal`, no mDNS, no Swarm/K8s DNS, no Tailscale ACL/join from the router.
- No brute-force walk of `100.64.0.0/10`. No `ipnet`/`cidr` crate unless std `Ipv4Addr` math is insufficient (it is sufficient for IPv4 `/16` max).
- Overlay `nodes:` stays a hard error. Discovery knobs live in tunables (`discovery:`), never in `fleet.yaml`.

## Decisions

### D1: `discovery:` tunables, default off

Add `DiscoveryConfig` to `YamlTunables` (`deny_unknown_fields`) and `router.defaults.yaml`:

```yaml
discovery:
  enabled: false
  cidrs: []
  tailscale: false
  tailscale_socket: /var/run/tailscale/tailscaled.sock
  agent_port: 11436
  ollama_ports: [11434, 11435]
  interval_seconds: 30
  probe_timeout_seconds: 0.5
  max_concurrent: 64
  forget_seconds: 180
```

Startup validation: `enabled` with empty `cidrs` and `tailscale: false` is a config error (no scan source). Each CIDR must parse as IPv4, prefix `16..=32`, not `0.0.0.0/0`. IPv6 prefixes (`::/0` and others) are rejected. Env knobs: `OLLAMA_ROUTER_DISCOVERY_ENABLED`, `OLLAMA_ROUTER_DISCOVERY_CIDRS` (CSV), `OLLAMA_ROUTER_DISCOVERY_TAILSCALE`. Reuse `health.capacity_probe_token` / `OLLAMA_CAPACITY_TOKEN` when GETting `/v1/status` if the agent requires a bearer.

*Why:* membership stays out of overlay YAML; operators opt in; `/16` caps a scan at 65k addresses (a `/24` is the expected swarm case).
*Rejected:* putting CIDRs in `fleet.yaml` (wrong file; GitOps pins are node ids). *Rejected:* default-on RFC1918 scan (too wide, surprises existing deploys).

### D2: Binary scan loop, core policy

Put HTTP/TCP probes and the periodic loop in `crates/ollama-router/src/discovery.rs`, spawned from `main.rs` next to `run_health` with the same `CancellationToken`. Pure pieces in `ollama-router-core`: CIDR expansion, origin-aware URL allowlist, `NodeOrigin::Discovered`, `Registry::upsert_discovered` / `forget_discovered`.

Concurrency: clone the health pattern — `tokio::sync::Semaphore` (`max_concurrent`) + `tokio::spawn` per target, `tokio::time::timeout` on each probe, `select!` on shutdown. No blocking I/O in Axum handlers; scan is a background task. Prefer `TcpStream::connect` then a short reqwest GET (existing rustls client) over a separate SYN-scanner.

Probe order per IP: agent port `GET /v1/status` (serde ignore extras, need `hostname`) → if that fails, skip with `no_agent` (do not adopt Ollama-only) → then each `ollama_ports` `GET /api/tags` or `/api/version` until one looks like Ollama.

Self-exclusion: skip `(ip, port)` equal to the process listen (`listen_host`/`listen_port` plus published host IPs). Also skip if `/healthz` JSON is the router `{status, version}` **and** `/readyz` exists (Ollama has no `/readyz`). Agent `/healthz` without `/v1/status` is not enough to adopt.

*Rejected:* adopting any open `:11434` (fleet-invariants + wiki: random laptops). *Rejected:* a second health-probe subsystem with different timeouts that counts as `inflight_inc`.

### D3: Tailscale peer list via LocalAPI, not CIDR

When `discovery.tailscale` is true, list peer IPv4s from Tailscale LocalAPI `GET /localapi/v0/status` over `discovery.tailscale_socket` (`tokio::net::UnixStream` + a small HTTP/1.1 GET, or hyper already in the reqwest graph — **do not** add `hyperlocal`). Probe only those IPs (union with CIDR targets). Missing socket / parse error → increment `tailscale_unavailable`, continue CIDRs, do not crash.

Swarm stack: optional bind-mount of the host socket into the router task (desktop-pc already has Tailscale). Overlay netns cannot run `tailscale` without the socket.

*Rejected:* scanning `100.64.0.0/10` (~4M addresses). *Rejected:* `tailscale status --json` subprocess (image has no Tailscale CLI). *Rejected:* router joining the tailnet.

### D4: Origin-aware URL policy (split CGNAT from public)

Keep `url_host_is_public_ip` for globally routable / unspecified / multicast. Add `url_host_is_cgnat` (`100.64/10`). Change `routing_url_blocked_reason(url, suffixes, origin)`:

| Host class | `permanent` / `discovered` / `adopt` | `verda` / `runpod` |
| --- | --- | --- |
| Loopback | allow | allow (zrok) |
| RFC1918 / ULA / link-local | allow | `public_url_blocked` |
| Tailscale CGNAT | allow | `public_url_blocked` |
| Global unicast IP | `public_url_blocked` | `public_url_blocked` |
| `*.zrok.io` + extras | `public_url_blocked` | `public_url_blocked` |

`hydrate_node_urls` must stop treating CGNAT as “public IP to replace with a tunnel” for permanent rows, so fleet.yaml Tailscale URLs stay direct HTTP. Cloud enroll still installs loopback zrok URLs.

*Rejected:* allowing CGNAT on cloud origins (breaks tunnel/loopback-only). *Rejected:* keeping CGNAT blocked for `permanent` (would leave swarm `100.x` rows unhealthy).

### D5: Identity and pin collision

Id = `NodeId::parse(hostname)` from `/v1/status` when valid; else `disc-<ipv4-with-dashes>`. `upsert_discovered`:

- No row → insert `Discovered` with URL/capacity_url only; `Capacity::default()` (unknown, not CPU zeros).
- Existing `Permanent` with URL → no-op (pin wins).
- Existing `Permanent` without URL → `FleetState` hydrate + `apply_permanent_config` URL fields; do not write `fleet.yaml`.
- Existing `Verda`/`Runpod` → no-op (do not convert cloud).
- Existing `Adopt`/`Discovered` → refresh URLs, touch `last_seen`.

`apply_permanent_inventory` continues to ignore non-`Permanent` rows, so a SIGHUP reload does not drop discovered hosts.

### D6: LAN enroll without shares (Axum 0.8 JSON)

Extend `EnrollOrigin` with `Discovered`. Make share ids `#[serde(default)] Option<String>` and add `ollama_url` / `capacity_url` options; keep `deny_unknown_fields`. Handler branches:

- Share ids present → existing zrok `ensure` path (`verda`/`runpod`/`fleet`/`adopt`).
- `origin: discovered` (or `adopt`) with direct URLs, empty shares → validate with D4 (`Adopt`/`Discovered`), `upsert_discovered` or `upsert_adopt`, persist FleetState routing URLs, **no** zrok access, **no** `fleet.yaml` write.
- Unset admin token → 403 as today.

Agent: if share ids exist, keep today’s JSON (including configured `register.origin`). Else build `http://{lan-or-cgnat}:{ollama_port}` and capacity port from `OLLAMA_NODE_AGENT_DISCOVERED_V4` / private IPv4 (existing `listen.rs`). LAN origin is `discovered` unless `register.origin` is explicitly `adopt`; never send `fleet`/`verda`/`runpod` on the no-share path. Skip when neither shares nor a usable address exist.

### D7: Forget is not destroy

Store `last_seen: Instant` on discovered (and LAN-enrolled discovered) rows. Successful scan hits and successful LAN enrolls refresh `last_seen`. When `discovery.enabled` is true, the discovery task sweeps: if `now - last_seen >= forget_seconds`, `forget_discovered` removes the registry row (drain-if-inflight like permanent remove, origin check = `Discovered` only). When `discovery.enabled` is false, do not run that sweep — heartbeat-created `discovered` rows persist until process restart (adopt-like). Do not call Verda/RunPod destroy. `idle_scale_down_candidates` already filters `origin == provider`; add a unit test that `Discovered` views are never returned for `Verda` or `Runpod`.

### D8: Metrics name split

Existing `ollama_router_discovery_total` counts tags/ps/version probes. New series: `ollama_router_host_discovery_events_total{event}` with `event` in `scan`, `found`, `adopted`, `forgotten`, `skipped_no_agent`, `skipped_self`, `skipped_public_url_blocked`, `skipped_outside_cidr`, `tailscale_unavailable`. No model-name labels. `/metrics` stays open.

### D9: Swarm example

`deploy/swarm/router.config.example.yaml`: `discovery.enabled: true`, `cidrs: ["192.168.100.0/24"]`, `tailscale: true`. `fleet.swarm.yaml` may become `nodes: []` or pin-only ids (`nuc` labels/capacity, no URL). Stack may mount `/var/run/tailscale/tailscaled.sock`. CI still never writes `fleet.yaml`. One replica, `stop-first` unchanged.

Console empty-state that currently dumps a `fleet.yaml` snippet SHOULD also mention enabling `discovery:` (docs/console, not a ranking change).

## Risks / Trade-offs

- **[Risk]** Overlay container cannot reach host `:11435` / Tailscale without the existing desktop-pc placement → **Mitigation:** keep placement constraint; document that discovery runs where the router already reaches backends.
- **[Risk]** `/16` scan + 0.5s timeout still takes time on empty subnets → **Mitigation:** TCP connect first, semaphore 64, skip `.0`/`.255`; default examples stay `/24`.
- **[Risk]** Agent token mismatch → silent `no_agent` skips → **Mitigation:** reuse `OLLAMA_CAPACITY_TOKEN`; metric `skipped_no_agent`; do not log the token.
- **[Risk]** Hostname collision with a Verda instance id → **Mitigation:** D5 refuses to convert cloud rows; operator uses `register.node_id`.
- **[Risk]** Behavior change: fleet.yaml `100.x` URLs become eligible instead of `public_url_blocked` → **Mitigation:** matches swarm intent; cloud origins still blocked; tests for both.
- **[Risk]** Unix LocalAPI HTTP is easy to get wrong → **Mitigation:** inject a `TailscalePeerSource` trait; unit-test with a fixture; soft-fail production.

## Migration Plan

1. Ship default `discovery.enabled: false` — existing `fleet.yaml` fleets unchanged except CGNAT permanent URLs may become healthy (desired).
2. Enable discovery in swarm example; operators can empty `url:` pins after agents listen on LAN.
3. Rollback: set `discovery.enabled: false` and restore `fleet.yaml` URLs; discovered rows vanish on restart (or after forget) without cloud destroys.
4. Do not require a FleetState schema migration beyond optional `managed_by=discovered` if URLs are persisted the same way as adopt.

## Open Questions

None that affect specs or task breakdown. Probe timeout / concurrency numbers above are defaults operators can override.
