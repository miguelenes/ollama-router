## Purpose

Lets the router find on-LAN and on-tailnet Ollama hosts by scanning configured CIDRs and enumerating Tailscale peers, so operators do not have to list those addresses in `fleet.yaml`.

## ADDED Requirements

### Requirement: Discovery is opt-in and bounded

Host discovery SHALL be off unless tunables set `discovery.enabled` and at least one scan source is configured: a CIDR list and/or Tailscale peer enumeration. `discovery.enabled: true` with neither a CIDR nor Tailscale enum MUST fail config load. Discovery MUST NOT probe the public Internet, MUST NOT accept `0.0.0.0/0` or `::/0`, MUST NOT accept other IPv6 prefixes, and MUST NOT brute-force scan `100.64.0.0/10`. Scan CIDRs are IPv4 only. Each configured IPv4 prefix MUST be no broader than `/16`. Discovery MUST NOT write `fleet.yaml`. Overlay YAML MUST still reject a top-level `nodes:` key.

#### Scenario: Disabled discovery probes nothing

- **WHEN** `discovery.enabled` is false (the default)
- **THEN** the router performs no CIDR probes and no Tailscale peer lookup for membership

#### Scenario: Open CIDR is rejected

- **WHEN** tunables list `0.0.0.0/0`, `::/0`, any other IPv6 prefix, or an IPv4 prefix broader than `/16`
- **THEN** config load fails with a validation error and the process does not start scanning

#### Scenario: Enabled with no scan source is rejected

- **WHEN** `discovery.enabled` is true and `cidrs` is empty and Tailscale enum is off
- **THEN** config load fails with a validation error and the process does not start scanning

#### Scenario: Empty fleet.yaml is valid with discovery

- **WHEN** discovery is enabled with at least one valid CIDR or Tailscale enum, and `fleet.yaml` is absent, empty `nodes: []`, or missing because `OLLAMA_ROUTER_FLEET` is unset
- **THEN** the router starts and membership comes from discovery and optional cloud enroll, and `fleet.yaml` is not created

### Requirement: CIDR scan adopts node-agent hosts only

When CIDRs are configured, the router SHALL periodically probe each address in those prefixes on the node-agent port (default `11436`). A host SHALL be adopted only when the agent answers `GET /v1/status` (hostname and identity come from that payload) and an Ollama listen is confirmed on a configured Ollama port (defaults `11434` and `11435`). Agent `/healthz` without `/v1/status` MUST NOT be enough to adopt. A listener that looks like this router process (`/healthz` plus `/readyz`, or the process listen address) MUST NOT be adopted. An Ollama port with no node-agent MUST NOT be adopted. Globally routable IPs, `*.zrok.io`, and other public-share hostnames MUST stay `public_url_blocked` and never become healthy.

#### Scenario: Agent plus Ollama on the CIDR joins

- **WHEN** discovery is enabled for `192.168.100.0/24` and a host at `192.168.100.160` serves node-agent on `:11436` and Ollama on `:11435`
- **THEN** that host appears in the live fleet as origin `discovered` with a direct HTTP URL on `:11435` and is eligible for holder ranking once healthy

#### Scenario: Bare Ollama without agent is ignored

- **WHEN** a CIDR host answers Ollama on `:11435` but nothing on the agent port
- **THEN** the host is not added to the fleet

#### Scenario: Router listen URL is not adopted

- **WHEN** the scan reaches the address and port this router process publishes as its Ollama-compatible listen URL
- **THEN** that target is skipped and no `discovered` row points at the router itself

### Requirement: Tailscale peers are enumerated, not scanned as a /10

When Tailscale peer enumeration is enabled, the router SHALL list current tailnet peer IPv4 addresses from the host Tailscale local API and probe only those addresses (plus configured LAN CIDRs). Missing Tailscale socket or API MUST soft-fail that source (reason-coded log and metric, no process crash) and MUST NOT disable CIDR scanning. Direct HTTP to RFC1918 and Tailscale CGNAT (`100.64.0.0/10`) SHALL be allowed for `discovered`, `permanent`, and `adopt` origins. Cloud origins MUST still require tunnel/loopback URLs; a Verda or RunPod node with an RFC1918, CGNAT, or public IP MUST remain `public_url_blocked`.

#### Scenario: Tailscale-only host is found via peer list

- **WHEN** Tailscale enum is on, the local API reports peer `100.98.93.14`, and that host runs node-agent plus Ollama
- **THEN** the host is adopted as origin `discovered` with a direct HTTP URL on that CGNAT address

#### Scenario: Tailscale API absent does not crash

- **WHEN** Tailscale enum is on but the local API socket is missing
- **THEN** the router stays up, CIDR scanning continues if configured, and a reason-coded discovery miss is recorded without secrets

#### Scenario: Cloud node cannot use Tailscale CGNAT as its Ollama URL

- **WHEN** a `verda` or `runpod` node would be recorded with `http://100.64.1.2:11434`
- **THEN** the URL is `public_url_blocked` and the node does not become healthy

#### Scenario: Cloud node cannot use RFC1918 as its Ollama URL

- **WHEN** a `verda` or `runpod` node would be recorded with `http://192.168.100.160:11434`
- **THEN** the URL is `public_url_blocked` and the node does not become healthy

### Requirement: Hostname identity collides with fleet.yaml pins

A discovered host's node id SHALL be the agent-reported hostname when it is a valid node id, otherwise a stable `disc-<ipv4-with-dashes>` id (for example `disc-192-168-100-160`). If that id already exists as a `fleet.yaml` `permanent` node, the router MUST NOT create a second row: a pin with a URL keeps that URL; a pin without a URL MAY hydrate reachability into FleetState and the live registry only. Discovery MUST NOT change labels or static capacity written in `fleet.yaml`. Matching an existing `verda` or `runpod` id MUST NOT convert that row to `discovered`.

#### Scenario: Pin with URL wins

- **WHEN** `fleet.yaml` lists `id: nuc` with `url: http://192.168.100.160:11435` and the scan finds the same hostname on a different address
- **THEN** the live node stays origin `permanent` with the fleet.yaml URL, and no `discovered` duplicate is created

#### Scenario: Pin without URL is hydrated

- **WHEN** `fleet.yaml` lists `id: nuc` with no URL and the scan finds hostname `nuc` at `http://192.168.100.160:11435`
- **THEN** the live node stays origin `permanent`, the URL is stored in FleetState (not `fleet.yaml`), and `fleet.yaml` is unchanged

### Requirement: Discovered members are forgotten, not destroyed

When `discovery.enabled` is true, a `discovered` node that is missing from scan and heartbeat for a configurable forget interval SHALL be removed from the live registry. Forget MUST NOT call Verda `delete_permanently`, MUST NOT terminate RunPod pods, MUST NOT drain-for-teardown, and MUST NOT write `fleet.yaml`. `permanent` and cloud rows MUST NOT be forgotten by this timer. When `discovery.enabled` is false, a heartbeat-created `discovered` row SHALL persist until process restart (it is not swept). Unknown VRAM on a newly discovered host MUST remain unknown (omitted), not encoded as a measured CPU (`vram_gb = 0`, `gpus = 0`) until the agent reports those fields.

#### Scenario: Host leaves the LAN

- **WHEN** a `discovered` node has been unseen by scan and heartbeat longer than the forget interval
- **THEN** it disappears from listing and ranking, and no cloud destroy API is called

#### Scenario: fleet.yaml host outage is not a forget

- **WHEN** a `permanent` node fails probes longer than the forget interval
- **THEN** it stays in inventory (unhealthy as today) and is not dropped by discovery forget

#### Scenario: Heartbeat discovered is not forgotten while discovery is off

- **WHEN** `discovery.enabled` is false and an agent enrolls `origin: discovered` over LAN URLs
- **THEN** the node stays in the live registry until the router process restarts and is not swept by the forget timer

### Requirement: Discovery metrics have no model-name labels

The router SHALL export `ollama_router_host_discovery_events_total{event}` for discovery scans, hosts found, hosts adopted, hosts skipped, and forgets. Skip `event` values SHALL include `skipped_no_agent`, `skipped_self`, `skipped_public_url_blocked`, `skipped_outside_cidr`, and `tailscale_unavailable`. Series MUST NOT carry model-name labels and MUST NOT reuse `ollama_router_discovery_total`. `/metrics` stays unauthenticated. Probe logs MUST NOT include request or response bodies, agent tokens, or the admin bearer.

#### Scenario: Skip reasons are countable

- **WHEN** a scan skips a bare Ollama host and the router listen URL in the same interval
- **THEN** `/metrics` shows skip increments with `event="skipped_no_agent"` and `event="skipped_self"` and no model-name label
