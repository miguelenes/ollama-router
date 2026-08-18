## ADDED Requirements

### Requirement: Discovered hosts are never destroyed

The autoscaler SHALL never destroy, stop, or drain-for-teardown a `discovered` origin host, regardless of idleness, envelopes, or provider state. Forget from the discovery timer is registry removal only and MUST NOT call a cloud destroy API.

#### Scenario: Idle discovered host is untouched by cloud teardown

- **WHEN** a `discovered` host has been idle far beyond every idle timeout
- **THEN** the reconcile loop performs no destroy or teardown action against it
