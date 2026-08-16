## ADDED Requirements

### Requirement: Unknown free VRAM is unknown, not zero

The system SHALL distinguish a node whose free VRAM has not been measured from a node whose agent reported 0 GiB free (a full GPU). Ranking MUST NOT treat omitted free VRAM as a measured `0`. When inflight/cap, RAM pressure, and GPU-util bias are equal, a holder with **known higher** free VRAM MUST rank ahead of a holder with unknown free VRAM, which MUST rank ahead of a holder with **known lower** free VRAM. Metrics MAY still publish a numeric 0 gauge when `vram_free_known` is false. Unknown free MUST NOT become a hard admission failure (agent-down nodes stay in the pool when other filters pass).

#### Scenario: unknown free is not treated as infinite headroom

- **WHEN** two healthy holders of the same model have equal inflight/cap, RAM pressure, and GPU-util bias, A has known 8 GiB free, and B has unknown free VRAM
- **THEN** ranking selects A first

#### Scenario: unknown free is not treated as a full GPU

- **WHEN** two healthy holders of the same model have equal inflight/cap, RAM pressure, and GPU-util bias, A has unknown free VRAM, and B has known 0 GiB free
- **THEN** ranking selects A first (B MUST NOT win as if unknown were 0)
