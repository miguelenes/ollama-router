## Purpose

Keeps size-and-load ranking and placement honest about hardware: omitted VRAM or GPU count is unknown, not a measured CPU with zero VRAM.

## ADDED Requirements

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
