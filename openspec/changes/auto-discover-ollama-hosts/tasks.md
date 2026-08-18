## 1. Config and URL policy

- [ ] 1.1 Add `discovery:` to `router.defaults.yaml` / `YamlTunables` / knobs (`OLLAMA_ROUTER_DISCOVERY_ENABLED`, `OLLAMA_ROUTER_DISCOVERY_CIDRS`, `OLLAMA_ROUTER_DISCOVERY_TAILSCALE`); default off; overlay `nodes:` still rejected
- [ ] 1.2 Validate: enabled requires a CIDR or Tailscale enum; reject `0.0.0.0/0`, `::/0`, other IPv6 prefixes, and IPv4 prefixes broader than `/16` (host-discovery “Open CIDR is rejected”, “Enabled with no scan source is rejected”, “Disabled discovery probes nothing”)
- [ ] 1.3 Split CGNAT from public in `url_policy`: `permanent`/`discovered`/`adopt` allow RFC1918 + `100.64/10` direct HTTP; `verda`/`runpod` still `public_url_blocked` on RFC1918/CGNAT/public/`*.zrok.io`; stop hydrating fleet.yaml CGNAT URLs into tunnels (host-discovery “Cloud node cannot use Tailscale CGNAT as its Ollama URL”, “Cloud node cannot use RFC1918 as its Ollama URL”)
- [ ] 1.4 Config tests: empty `fleet.yaml` / missing default path + discovery enabled starts; enabled with no sources fails; `task compose:mock` inventory comments stay 0/0 = known CPU, omit = unknown

## 2. Registry origin and idle teardown

- [ ] 2.1 Add `NodeOrigin::Discovered` (`as_str() == "discovered"`); `upsert_discovered` / `forget_discovered`; `apply_permanent_inventory` must not drop discovered rows
- [ ] 2.2 Pin collision: same hostname as fleet.yaml id with URL → keep `permanent` URL, no duplicate; pin without URL → FleetState hydrate only, do not write `fleet.yaml` (host-discovery “Pin with URL wins”, “Pin without URL is hydrated”)
- [ ] 2.3 Insert discovered nodes with omitted capacity (unknown), not `vram_gb = 0` / `gpus = 0`
- [ ] 2.4 Cloud idle/orphan unit tests: `Discovered` never appears in `idle_scale_down_candidates` / destroy order for Verda or RunPod (cloud-autoscale “Idle discovered host is untouched by cloud teardown”; admin-nodes “Scan-created node is not Verda”)

## 3. CIDR scan loop

- [ ] 3.1 Pure CIDR expand in core (std `Ipv4Addr`, skip network/broadcast); no new CIDR crate
- [ ] 3.2 `crates/ollama-router/src/discovery.rs`: Tokio `Semaphore` + spawn + reqwest timeouts (mirror `health.rs`); probe agent `/v1/status` then configured Ollama ports; spawn from `main.rs` with shutdown; never `inflight_inc`
- [ ] 3.3 httpmock: agent+Ollama on CIDR → origin `discovered` and ranking-eligible once healthy (host-discovery “Agent plus Ollama on the CIDR joins”)
- [ ] 3.4 httpmock: Ollama without agent is not adopted (`skipped_no_agent`) (host-discovery “Bare Ollama without agent is ignored”)
- [ ] 3.5 Skip the router listen URL / `/readyz` identity (`skipped_self`) (host-discovery “Router listen URL is not adopted”)
- [ ] 3.6 Forget timer (only while `discovery.enabled`) removes `discovered` rows unseen by scan and heartbeat; no cloud destroy API; `permanent` outage is not forgotten; heartbeat-only discovered with discovery off persists until restart (host-discovery “Host leaves the LAN”, “fleet.yaml host outage is not a forget”, “Heartbeat discovered is not forgotten while discovery is off”)

## 4. Tailscale peer enumeration

- [ ] 4.1 `TailscalePeerSource` trait + Unix LocalAPI `GET /localapi/v0/status` (no `hyperlocal`, no `tailscale` CLI); fixture returns `100.98.93.14`
- [ ] 4.2 Probe enumerated peers only; missing socket soft-fails `tailscale_unavailable` and does not stop CIDR scan (host-discovery “Tailscale-only host is found via peer list”, “Tailscale API absent does not crash”)

## 5. LAN enroll and agent heartbeat

- [ ] 5.1 Axum 0.8 `EnrollRequest`: `EnrollOrigin::Discovered`; optional share ids; `ollama_url` / `capacity_url`; keep `deny_unknown_fields`; fail-closed admin bearer
- [ ] 5.2 LAN path: RFC1918/CGNAT URLs without shares upsert discovered (or adopt); public/`*.zrok.io` → `public_url_blocked`; unset token → 403; never write `fleet.yaml` (admin-nodes “LAN heartbeat enrolls without shares”, “Public LAN enroll is refused”, “Unset admin token still fails closed”, “Enroll discovered does not write inventory”)
- [ ] 5.3 Share-token enroll for verda/runpod/fleet unchanged
- [ ] 5.4 Node-agent: if shares present, existing payload; else LAN URLs from private/CGNAT IPv4 with `origin: discovered` unless `register.origin` is `adopt` (never `fleet`/`verda`/`runpod` on the no-share path); skip when neither exists; no secrets in logs (node-agent “LAN agent enrolls with direct URLs”, “LAN agent may enroll as adopt when configured”, “No address and no shares skips the beat”, “Share heartbeat still wins on tunneled hosts”)

## 6. Admin, metrics, console, OpenAPI

- [ ] 6.1 Readiness counts include `discovered` separately from `permanent`/`verda` (admin-nodes “Discovered nodes are counted separately”; keep RunPod/adopt scenarios green)
- [ ] 6.2 CLI `nodes` + `node_info` origin `discovered`; console empty-state mentions discovery CIDRs as well as fleet.yaml pins
- [ ] 6.3 `ollama_router_host_discovery_events_total{event}` for scan/found/adopted/forgotten/skip reasons; no model-name labels; do not overload `ollama_router_discovery_total` (host-discovery “Skip reasons are countable”)
- [ ] 6.4 OpenAPI enroll schema: optional shares + LAN URLs + `origin: discovered`

## 7. Swarm example and public docs

- [ ] 7.1 Swarm example tunables: `discovery.enabled`, `cidrs: ["192.168.100.0/24"]`, `tailscale: true`; optional Tailscale socket mount; `fleet.swarm.yaml` empty or pin-only; CI still never writes fleet.yaml; replicas remain 1 (swarmnet-local-deploy scenarios)
- [ ] 7.2 Site fleet guide + quick start: optional `fleet.yaml` pins, CIDR/Tailscale discovery, agent-only adopt, enroll never writes the file; cloud guide still Verda+RunPod only (public-docs-site scenarios)

## 8. Wiki, rules, gate

- [ ] 8.1 Update `.opencode/wiki/concepts/ollama-router-product.md` and `ollama-capacity-discovery.md`: optional fleet.yaml, scan+heartbeat, CGNAT for non-cloud, heartbeat no longer share-only
- [ ] 8.2 Update `.cursor/rules/fleet-invariants.mdc` and `openspec/config.yaml` context: inventory is fleet.yaml pins **or** discovery + FleetState + cloud; still never write fleet.yaml from enroll
- [ ] 8.3 Sequential `task check` then `task coverage` (fmt, clippy `-D warnings`, test `--locked`, deny, llvm-cov ≥ 80%); no Makefile/justfile/npm scripts
