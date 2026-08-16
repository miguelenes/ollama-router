# cloud-provider-runpod Specification

## Purpose
RunPod-specific pod lifecycle behind the generic cloud-autoscale contract: catalog-driven selection that implements the shared value-for-money and hourly-cap rules, interruptible-first rental, container-based bootstrap whose Ollama URL is only ever a loopback zrok private share, permanent termination on teardown, and spot-interruption replacement.

## Requirements

### Requirement: RunPod tunables block and fail-closed credentials
The system SHALL accept a `runpod:` tunables block in the layered YAML config, disabled by default. The API key MUST be sourced from an environment variable (default `RUNPOD_API_KEY`) named by `api_key_env`; keys or tokens in YAML MUST be rejected as unknown fields. Enabling RunPod without the key present MUST be a hard startup configuration error. A `thunder:` block MUST remain an unknown-field configuration error.

#### Scenario: RunPod overlay parses
- **WHEN** an overlay sets `runpod.enabled: true` with valid tunables and `RUNPOD_API_KEY` is set
- **THEN** configuration loads and the provider starts

#### Scenario: Enabled without credentials fails closed
- **WHEN** `runpod.enabled: true` and the configured API key env var is unset
- **THEN** startup fails with a configuration error naming the missing env var (never a default key)

#### Scenario: Thunder stays rejected
- **WHEN** an overlay contains a top-level `thunder:` block
- **THEN** configuration loading fails with an unknown-field error

### Requirement: Best price-per-VRAM GPU selection
The system SHALL select the GPU type for a new pod from the RunPod catalog under the shared cloud-autoscale value-for-money and hourly-cap rules. Only offers that are currently available, whose VRAM lies within the configured band (`min_vram_gb`..`max_vram_gb`), that pass allow/deny type lists and data-center filters, and whose effective hourly price does not exceed `max_price_per_hour` (when set) are eligible. Among eligible offers the system SHALL rank by lowest price per VRAM GiB and request GPU types in that order so the marketplace fills the best-value offer first. Because the catalog may not quote interruptible prices, the cap MUST also be verified against the created pod's actual `costPerHr`; an over-cap pod MUST be terminated immediately and counted as a stockout.

#### Scenario: Cheapest per-GiB eligible GPU wins
- **WHEN** two available GPU types are in-band and under the price cap, and type A has a lower price per VRAM GiB than type B
- **THEN** the pod create requests type A ahead of type B

#### Scenario: Over-cap GPU is skipped even if it would win on value
- **WHEN** type A has a lower price per VRAM GiB than type B but type A's hourly price exceeds `max_price_per_hour`
- **THEN** type A is not requested and type B is considered instead

#### Scenario: Nothing eligible means no create
- **WHEN** no available GPU type is in-band and under the price cap
- **THEN** no pod is created, the miss is logged as a reason code, and the client keeps receiving 503 with `Retry-After`

### Requirement: Interruptible-first rental
The system SHALL create interruptible (spot) pods when spot preference is enabled (the default). Falling back to on-demand rental MUST be an explicit opt-in tunable and MUST only happen after interruptible availability is exhausted for every eligible GPU type.

#### Scenario: Spot pod is created
- **WHEN** scale-up selects RunPod with spot preference enabled and interruptible capacity exists
- **THEN** the pod is created as interruptible

#### Scenario: No silent on-demand fallback
- **WHEN** interruptible capacity is exhausted and on-demand fallback is not enabled
- **THEN** no on-demand pod is created and the stockout is logged as a reason code

### Requirement: Container bootstrap with tunnel-only URLs
RunPod pods SHALL be bootstrapped by running a router-specified container image whose environment carries the node-agent enroll configuration and zrok material; the router MUST NOT SSH into pods. The pod's Ollama URL becomes healthy only after enroll of a loopback zrok private share via `POST /router/v1/nodes/enroll`. The pod's public IP, RunPod proxy hostname, or any non-loopback URL MUST be rejected as `public_url_blocked` and never become healthy. Enroll MUST NOT write `fleet.yaml`.

#### Scenario: Enrolled loopback share becomes healthy
- **WHEN** a pod's node-agent enrolls a loopback zrok private share URL
- **THEN** the node becomes routable after health checks pass

#### Scenario: Public pod URL is blocked
- **WHEN** an enroll or state row carries the pod's public IP or a RunPod proxy hostname
- **THEN** the URL is rejected as `public_url_blocked` and the node is never marked healthy

### Requirement: Managed-marker ownership and adoption safety
The system SHALL mark every pod it creates with a router-managed identifier (pod naming scheme carrying the router id, since RunPod has no instance tags) and SHALL manage only pods carrying its own marker. Pods without the marker MUST never be adopted, stopped, or destroyed. FleetState rows for RunPod pods carry `managed_by=runpod`; enroll with `origin=runpod` MUST only update an existing `managed_by=runpod` row.

#### Scenario: Foreign pod is untouched
- **WHEN** the RunPod account contains pods not created by this router
- **THEN** reconcile never destroys, stops, or adopts them

#### Scenario: Enroll cannot invent a RunPod node
- **WHEN** an enroll request claims `origin=runpod` for a node id with no existing `managed_by=runpod` FleetState row
- **THEN** the enroll is rejected

### Requirement: Teardown terminates permanently
Scale-down, idle teardown, interruption cleanup, and orphan reclaim SHALL permanently terminate RunPod pods (releasing compute and pod-local storage). The system MUST NOT leave pods in the stopped state, because stopped pods continue to bill volume storage.

#### Scenario: Idle teardown terminates
- **WHEN** the autoscaler tears down an idle RunPod pod
- **THEN** the pod is terminated permanently (not stopped) and its FleetState row is removed after success

### Requirement: Spot interruption replacement
A managed interruptible pod observed outside the running state (interrupted, exited, or errored) SHALL be treated as unavailable, terminated permanently, and replaced only through the standard paths: immediately when the provider is below its floor, otherwise via the coalesced demand path on the next capacity miss.

#### Scenario: Interrupted pod below floor is replaced
- **WHEN** a spot pod is interrupted and the managed count falls below `min_instances`
- **THEN** the pod is terminated and a replacement create is issued by reconcile without client load

#### Scenario: Interrupted pod above floor waits for demand
- **WHEN** a spot pod is interrupted while the remaining managed count still meets the floor
- **THEN** the pod is terminated and no replacement is created until a capacity miss triggers the demand path

### Requirement: Lenient provider payloads and sensitive-data handling
RunPod API responses SHALL be parsed ignoring unknown fields so provider-side additions never break the router. The system MUST NOT log or persist the RunPod API key, pod environment contents (zrok enable token, enroll bearer), or raw provider response bodies. Logged fields are limited to the allowlist: pod id, GPU type, data center, price, VRAM GiB, status, and reason codes.

#### Scenario: Unknown fields are ignored
- **WHEN** a RunPod response contains fields the router does not model
- **THEN** parsing succeeds and the operation continues

#### Scenario: Create failure logs no secrets
- **WHEN** a pod create fails
- **THEN** the log carries a reason code and allowlisted fields only, never the request body, API key, or pod env

### Requirement: RunPod nodes visible in admin surfaces
RunPod-managed nodes SHALL appear in the admin nodes listing and CLI `nodes` output with `origin=runpod`, node id, URL, tunnel backend, and enroll age, and MUST never expose share tokens or the API key. The `ollama_router_node_info` gauge SHALL carry the `runpod` origin label value.

#### Scenario: Nodes listing shows RunPod origin
- **WHEN** an operator lists nodes while a RunPod pod is enrolled
- **THEN** the row shows `origin=runpod` with id, URL, tunnel backend, and enroll age, and no token material
