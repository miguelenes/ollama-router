# size-load-routing Specification

## Purpose
Keeps size-and-load ranking and placement honest about hardware: omitted VRAM or GPU count is unknown, not a measured CPU with zero VRAM.

## Requirements

### Requirement: Omitted VRAM is unknown, not zero

The system SHALL distinguish a node whose VRAM (or GPU count) has not been measured or statically declared from a node whose inventory reports zero VRAM or zero GPUs. Ranking, placement static VRAM gates, and class VRAM preference MUST NOT treat omitted VRAM as a measured `0`. Health probes MAY still mark the node healthy from `/api/tags` when the capacity agent is down.

#### Scenario: omitted VRAM is not encoded as zero

- **WHEN** a node has no static or agent VRAM (and no GPU count)
- **THEN** ranking and placement MUST NOT treat that node as a measured CPU with VRAM 0 / `gpus = 0`

### Requirement: MEDIUM and LARGE require known sufficient VRAM

The system SHALL admit MEDIUM inference and MEDIUM placement only to nodes whose **known** VRAM meets the configured medium minimum. The system SHALL admit LARGE inference and LARGE placement only to nodes whose **known** VRAM meets the LARGE static estimate. A node with unknown VRAM MUST NOT satisfy those gates. When every inference holder fails those gates, the client MUST receive 503 with reason `insufficient_capacity` (existing capacity-miss Retry-After policy). EMBED, SMALL, and GENERIC inference MUST remain admissible on unknown-VRAM holders when other filters pass.

#### Scenario: LARGE against unknown-only holders

- **WHEN** the only healthy holders of a LARGE-class model have unknown VRAM
- **THEN** generate/chat for that model is 503 `insufficient_capacity` and is not forwarded

#### Scenario: LARGE against a known GPU

- **WHEN** a healthy holder of that LARGE model has known VRAM at or above the LARGE estimate
- **THEN** ranking MAY select that holder (subject to health, saturation, and RAM filters)

#### Scenario: SMALL still runs on unknown capacity

- **WHEN** the only healthy holder of a SMALL model has unknown VRAM
- **THEN** generate/chat for that model is forwarded to that holder if it is healthy and not saturated

### Requirement: Inflight caps stay conservative when VRAM is unknown

When VRAM is known, the system SHALL keep the existing VRAM-tier default inflight caps (and explicit `max_inflight` / fleet `default_max_inflight` overrides). When VRAM is unknown and no explicit cap is set, the system MAY use the lowest-tier numeric cap as a conservative default. That numeric coincidence MUST NOT cause the node to be treated as a measured CPU for SMALL GPU preference or as `vram = 0` for MEDIUM/LARGE gates.

#### Scenario: unknown VRAM still fails LARGE

- **WHEN** a healthy holder has unknown VRAM and no explicit inflight cap, and the request is LARGE
- **THEN** that holder does not pass the LARGE VRAM gate, even if its inflight cap number matches a small-GPU node

### Requirement: Unknown GPU utilization is unknown, not zero

The system SHALL distinguish a node whose GPU utilization has not been measured from a node whose agent reported 0% busy. Ranking MUST NOT treat omitted GPU util as a measured idle `0`. When inflight/cap and RAM pressure are equal, a holder with **known lower** GPU util MUST rank ahead of a holder with unknown GPU util, which MUST rank ahead of a holder with **known higher** GPU util. Metrics MAY still publish a numeric 0 gauge when `gpu_util_known` is false.

#### Scenario: unknown util is not preferred as idle

- **WHEN** two healthy holders of the same model have equal inflight/cap and RAM pressure, A has known GPU util 10%, and B has unknown GPU util
- **THEN** ranking selects A first (B MUST NOT win as if util were 0)

#### Scenario: unknown util is not worse than a busy GPU

- **WHEN** two healthy holders of the same model have equal inflight/cap and RAM pressure, A has unknown GPU util, and B has known GPU util 90%
- **THEN** ranking selects A first

### Requirement: Unknown free VRAM is unknown, not zero

The system SHALL distinguish a node whose free VRAM has not been measured from a node whose agent reported 0 GiB free (a full GPU). Ranking MUST NOT treat omitted free VRAM as a measured `0`. When inflight/cap, RAM pressure, and GPU-util bias are equal, a holder with **known higher** free VRAM MUST rank ahead of a holder with unknown free VRAM, which MUST rank ahead of a holder with **known lower** free VRAM. Metrics MAY still publish a numeric 0 gauge when `vram_free_known` is false. Unknown free MUST NOT become a hard admission failure (agent-down nodes stay in the pool when other filters pass).

#### Scenario: unknown free is not treated as infinite headroom

- **WHEN** two healthy holders of the same model have equal inflight/cap, RAM pressure, and GPU-util bias, A has known 8 GiB free, and B has unknown free VRAM
- **THEN** ranking selects A first

#### Scenario: unknown free is not treated as a full GPU

- **WHEN** two healthy holders of the same model have equal inflight/cap, RAM pressure, and GPU-util bias, A has unknown free VRAM, and B has known 0 GiB free
- **THEN** ranking selects A first (B MUST NOT win as if unknown were 0)

### Requirement: Unknown CPU utilization is unknown, not zero

The system SHALL distinguish a node whose CPU utilization has not been measured from a node whose agent reported a low busy percentage. Ranking MUST NOT treat omitted CPU util as a measured idle `0`. When inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias are equal, a holder with **known lower** CPU util MUST rank ahead of a holder with unknown CPU util, which MUST rank ahead of a holder with **known higher** CPU util. Metrics MAY still publish a numeric 0 gauge when the CPU sample is unknown, but the rank path MUST NOT read it as idle.

#### Scenario: unknown CPU util is not preferred as idle

- **WHEN** two healthy holders of the same model are equal on inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias, A has known CPU util 10%, and B has unknown CPU util
- **THEN** ranking selects A first (B MUST NOT win as if CPU util were 0)

#### Scenario: unknown CPU util is not worse than a pinned CPU

- **WHEN** two healthy holders of the same model are equal on inflight/cap, RAM pressure, GPU-util bias, and free-VRAM bias, A has unknown CPU util, and B has known CPU util 95%
- **THEN** ranking selects A first
