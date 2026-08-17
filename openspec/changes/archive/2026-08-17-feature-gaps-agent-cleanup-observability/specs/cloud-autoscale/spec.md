## MODIFIED Requirements

### Requirement: Per-provider scale observability

The system SHALL export Prometheus metrics for managed-instance count, creates, destroys, and scale decisions labeled by provider and reason. Each provider event metric SHALL carry a `reason` label describing why the event occurred — the triggering route reason on demand events, the failure class on `ensure_failed`/destroy failures, and the empty string when no reason applies, so label-agnostic selectors keep matching. These metrics MUST NOT carry model-name labels and MUST NOT expose tokens, URLs, or share secrets.

#### Scenario: Scale decisions are visible per provider

- **WHEN** a create and a destroy occur on different providers
- **THEN** the metrics endpoint shows each event attributed to its provider without secret material

#### Scenario: Demand records the trigger reason

- **WHEN** a MEDIUM capacity miss triggers a coalesced create on a provider
- **THEN** the demand event carries the provider label and a reason label holding the triggering route reason (for example `insufficient_capacity`)

#### Scenario: Compose dashboards and the ensure-failed alert follow provider labels

- **WHEN** both providers have created instances and one reports `ensure_failed`
- **THEN** the fleet home, Cloud, and nodes compose dashboards show instance count, hourly price, and events split by provider, and the ensure-failed alert fires with a provider label
