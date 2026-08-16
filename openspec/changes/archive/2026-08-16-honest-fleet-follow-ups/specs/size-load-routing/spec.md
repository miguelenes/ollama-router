## ADDED Requirements

### Requirement: Unknown GPU utilization is unknown, not zero

The system SHALL distinguish a node whose GPU utilization has not been measured from a node whose agent reported 0% busy. Ranking MUST NOT treat omitted GPU util as a measured idle `0`. When inflight/cap and RAM pressure are equal, a holder with **known lower** GPU util MUST rank ahead of a holder with unknown GPU util, which MUST rank ahead of a holder with **known higher** GPU util. Metrics MAY still publish a numeric 0 gauge when `gpu_util_known` is false.

#### Scenario: unknown util is not preferred as idle

- **WHEN** two healthy holders of the same model have equal inflight/cap and RAM pressure, A has known GPU util 10%, and B has unknown GPU util
- **THEN** ranking selects A first (B MUST NOT win as if util were 0)

#### Scenario: unknown util is not worse than a busy GPU

- **WHEN** two healthy holders of the same model have equal inflight/cap and RAM pressure, A has unknown GPU util, and B has known GPU util 90%
- **THEN** ranking selects A first
