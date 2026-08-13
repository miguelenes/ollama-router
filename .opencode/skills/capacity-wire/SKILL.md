---
name: capacity-wire
description: Implements the router HTTP client for the sibling ollama-capacity-agent on :11436 (GET /v1/capacity and /v1/pressure), GiB = 1024³, and soft-fail when the agent is down. Use when changing capacity probes, RAM pressure merge, or capacity fixtures.
---

# Capacity wire

Read `.opencode/wiki/concepts/ollama-capacity-discovery.md`.
Router client: `crates/ollama-router-core/src/capacity/`.
Agent (do not reimplement):
`/home/menes/Projects/illumination/services/ollama-capacity-agent/`.

## Endpoints

| Method | Path | Auth |
|--------|------|------|
| GET | `/healthz` | none |
| GET | `/v1/capacity` | bearer if `OLLAMA_CAPACITY_TOKEN` set |
| GET | `/v1/pressure` | same |

Default URL: `http://{ollama-host}:11436`. Override `OLLAMA_HOST_NN_CAPACITY_URL`.

Probe **after** a successful Ollama `/api/tags` health check.

## GiB

`bytes as f64 / (1024.0 * 1024.0 * 1024.0)`. sysinfo returns **bytes**.
Dividing by `1024²` inflates figures 1024× and breaks RAM thresholds.

## Soft-fail

Agent unreachable → node stays healthy if `/api/tags` works; keep last
discovered capacity; set `capacity_error`; degrade to static / `ps_lower_bound`.
Never flip the node unhealthy solely because `:11436` is down.

## Merge

Effective capacity fills omitted static fields from discovery. Explicit static
VRAM/RAM **cap** discovered values. Router **reclassifies** `pressure_level`
with its own policy; the agent's level is an ops hint.

## Tests

httpmock + fixtures matching the agent JSON. Assert `32 * 1024³` bytes → 32.0 GiB.
No live hosts required.
