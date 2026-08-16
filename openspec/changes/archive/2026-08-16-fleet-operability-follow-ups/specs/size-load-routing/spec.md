## ADDED Requirements

### Requirement: Unknown CPU utilization is unknown, not zero

The system SHALL distinguish a node whose CPU utilization has not been measured from a node whose agent reported a low busy percentage. Ranking MUST NOT treat omitted CPU util as a measured idle `0`. When inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias are equal, a holder with **known lower** CPU util MUST rank ahead of a holder with unknown CPU util, which MUST rank ahead of a holder with **known higher** CPU util. Metrics MAY still publish a numeric 0 gauge when the CPU sample is unknown, but the rank path MUST NOT read it as idle.

#### Scenario: unknown CPU util is not preferred as idle

- **WHEN** two healthy holders of the same model are equal on inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias, A has known CPU util 10%, and B has unknown CPU util
- **THEN** ranking selects A first (B MUST NOT win as if CPU util were 0)

#### Scenario: unknown CPU util is not worse than a pinned CPU

- **WHEN** two healthy holders of the same model are equal on inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias, A has unknown CPU util, and B has known CPU util 95%
- **THEN** ranking selects A first
