## Purpose

Provider-agnostic contract for growing and shrinking the cloud slice of the fleet: per-provider min/max envelopes, scale-up only under real client load, idle scale-down, billing-aware teardown timing, best value-for-money instance selection with a user-defined hourly price cap, and provider isolation — independent of which GPU marketplace supplies the instance.

## ADDED Requirements

### Requirement: Per-provider autoscale envelope
The system SHALL maintain, for each enabled cloud provider, at least `min_instances` and at most `max_instances` router-managed instances. The floor SHALL be satisfied by the reconcile loop without requiring client load; the ceiling MUST NOT be exceeded even under sustained demand. Envelope counts include only provider-managed instances — never `fleet.yaml` hosts.

#### Scenario: Floor is satisfied without load
- **WHEN** a provider is enabled with `min_instances: 1` and zero managed instances exist
- **THEN** the reconcile loop creates one instance even though no client request has arrived

#### Scenario: Ceiling is respected under load
- **WHEN** a provider already has `max_instances` managed instances and a capacity miss occurs
- **THEN** no additional create is issued and the client receives 503 with `Retry-After`

### Requirement: Scale-up above the floor is load-only and coalesced
The system SHALL create instances above the floor only in response to real client demand (capacity miss or all-holders-saturated on generate, chat, embed, or OpenAI `/v1` inference). Concurrent triggers MUST coalesce into at most one in-flight create per provider. The triggering client MUST receive 503 with `Retry-After` immediately; requests MUST NOT block on provisioning.

#### Scenario: Concurrent misses coalesce
- **WHEN** many concurrent capacity misses occur for the same provider
- **THEN** at most one instance create is in flight for that provider

#### Scenario: Non-client traffic never scales up
- **WHEN** health probes, `/api/ps`, capacity probes, admin calls, or warm-keeper requests occur
- **THEN** no scale-up is triggered and `last_client_request_at` is unchanged

### Requirement: Value-for-money instance selection
Each enabled cloud provider SHALL select among currently eligible offers by lowest price per known VRAM GiB (best value). An offer with unknown VRAM MUST NOT win a value comparison against an offer with known VRAM. A provider MAY expose an explicit `cheapest` strategy that ranks by hourly price instead; the default MUST be best value.

#### Scenario: Better value wins over cheaper sticker
- **WHEN** two in-band offers are under the hourly cap, offer A is $0.40/hr for 48 GiB and offer B is $0.20/hr for 8 GiB
- **THEN** the provider creates offer A (lower price per VRAM GiB)

#### Scenario: Unknown VRAM loses a value comparison
- **WHEN** one eligible offer has known VRAM and another has unknown VRAM
- **THEN** the known-VRAM offer is selected

### Requirement: User-defined hourly price cap
Each provider SHALL accept an optional operator-configured maximum hourly price. Offers whose effective hourly price exceeds the cap MUST be ineligible. When the cap is unset, no price-cap filter applies. When every otherwise-eligible offer exceeds the cap, the provider MUST NOT create and MUST log a reason code; the client keeps receiving 503 with `Retry-After`.

#### Scenario: Over-cap offer is skipped
- **WHEN** the operator sets a $0.50/hr cap and the only better-value GPU costs $0.80/hr
- **THEN** that GPU is not created

#### Scenario: Everything over cap means no create
- **WHEN** every otherwise-eligible offer exceeds the configured hourly cap
- **THEN** no instance is created, the miss is logged as a reason code, and the client receives 503 with `Retry-After`

### Requirement: Cross-provider choice by value
When more than one provider is enabled and below its ceiling at scale-up time, the system SHALL create on the provider whose best currently eligible offer has the lowest price per known VRAM GiB, and SHALL fall back to the next provider when that one has no eligible availability. Equal value scores MUST break ties by lower hourly price. A provider at its ceiling or with no eligible offer MUST be skipped without aborting scale-up.

#### Scenario: Better-value provider wins
- **WHEN** two providers are enabled below ceiling and provider A's best eligible offer has a lower price per VRAM GiB than provider B's
- **THEN** the new instance is created on provider A

#### Scenario: Stockout falls back
- **WHEN** the best-value provider reports no eligible availability
- **THEN** the create proceeds on the next-best-value enabled provider below its ceiling

### Requirement: Idle scale-down above the floor
The system SHALL destroy managed instances above the floor after `idle_timeout` elapses with no client forwards, where idleness derives solely from `last_client_request_at` written by `inflight_inc` on generate, chat, embed, and OpenAI `/v1` inference endpoints. A post-create grace period MUST protect new instances from immediate teardown. Instances at or below the floor MUST NOT be destroyed for idleness.

#### Scenario: Idle instance above floor is destroyed
- **WHEN** a managed instance above the floor has had no client forwards for `idle_timeout` and its post-create grace has passed
- **THEN** the reconcile loop destroys it

#### Scenario: Floor instance is retained while idle
- **WHEN** the managed count equals `min_instances` and the idle timeout elapses
- **THEN** no instance is destroyed

### Requirement: Billing-aware teardown timing
The system SHALL support a per-provider minimum billed lifetime. An idle instance MUST NOT be destroyed before that lifetime has elapsed. For a per-second-billed provider the minimum lifetime MAY be zero and teardown happens as soon as idle + grace allow.

#### Scenario: Minimum lifetime is honored
- **WHEN** an instance is idle past `idle_timeout` but younger than the provider's configured minimum billed lifetime
- **THEN** it is retained until that lifetime has elapsed

#### Scenario: Fine-grained billing tears down promptly
- **WHEN** a per-second-billed provider's instance is idle past `idle_timeout` and grace, with zero minimum lifetime
- **THEN** it is destroyed on the next reconcile pass

### Requirement: fleet.yaml hosts are never destroyed
The autoscaler SHALL never destroy, stop, or drain-for-teardown a `fleet.yaml` inventory host, regardless of idleness, envelopes, or provider state.

#### Scenario: Idle fleet.yaml host is untouched
- **WHEN** a `fleet.yaml` host has been idle far beyond every idle timeout
- **THEN** the reconcile loop performs no destroy or teardown action against it

### Requirement: Orphan reclaim and failed-destroy retention
The system SHALL reclaim provider instances that carry the router-managed marker but have no FleetState row after a configurable grace period. A destroy that fails MUST retain the FleetState ownership row so teardown is retried on a later pass.

#### Scenario: Orphaned managed instance is reclaimed
- **WHEN** a provider lists a router-managed instance with no FleetState row for longer than the orphan grace
- **THEN** the instance is permanently destroyed

#### Scenario: Failed destroy is retried
- **WHEN** a destroy call fails
- **THEN** the FleetState ownership row is retained and the destroy is attempted again on a later reconcile pass

### Requirement: Provider isolation and soft-fail
A provider API outage, auth failure, or misconfiguration SHALL NOT stop the reconcile loop, affect the other provider's scaling, or change the health of `fleet.yaml` nodes. Provider errors MUST surface as reason-coded logs and metrics without secrets or response bodies.

#### Scenario: One provider down, the other still scales
- **WHEN** provider A's API is unreachable and a capacity miss occurs
- **THEN** provider B (enabled, below ceiling) still receives the coalesced create and provider A's failure is logged as a reason code only

### Requirement: Per-provider scale observability
The system SHALL export Prometheus metrics for managed-instance count, creates, destroys, and scale decisions labeled by provider (and reason where applicable). These metrics MUST NOT carry model-name labels and MUST NOT expose tokens, URLs, or share secrets.

#### Scenario: Scale decisions are visible per provider
- **WHEN** a create and a destroy occur on different providers
- **THEN** the metrics endpoint shows each event attributed to its provider without secret material

#### Scenario: Compose dashboards and the ensure-failed alert follow provider labels
- **WHEN** both providers have created instances and one reports `ensure_failed`
- **THEN** the fleet home, Cloud, and nodes compose dashboards show instance count, hourly price, and events split by provider, and the ensure-failed alert fires with a provider label
