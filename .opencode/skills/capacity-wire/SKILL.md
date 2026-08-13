---
name: capacity-wire
description: Implements the router HTTP client for ollama-node-agent on :11436 (GET /v1/capacity and /v1/pressure), GiB = 1024³, and soft-fail when the agent is down. Use when changing capacity probes, RAM pressure merge, or capacity fixtures.
---

# Capacity wire

Read `.opencode/wiki/concepts/ollama-capacity-discovery.md`.
Router client: `crates/ollama-router-core/src/capacity/`.
Shared DTOs: `crates/ollama-capacity-types/`.
Agent: `crates/ollama-node-agent/` (`setup` elevated, `serve` unprivileged).

## Endpoints

| Method | Path | Auth |
|--------|------|------|
| GET | `/healthz` | none |
| GET | `/metrics` | none |
| GET | `/v1/capacity` | bearer if agent `token` / `OLLAMA_NODE_AGENT_TOKEN` set |
| GET | `/v1/pressure` | same |
| GET | `/v1/status` | same |

Default URL: `http://{ollama-host}:11436`. Override per node with fleet.yaml `capacity_url`.

Probe **after** a successful Ollama `/api/tags` health check.

## GiB

`bytes as f64 / (1024.0 * 1024.0 * 1024.0)`. sysinfo returns **bytes**.
Dividing by `1024²` inflates figures 1024× and breaks RAM thresholds.
nvidia-smi memory columns are MiB (`/ 1024` → GiB).

## Soft-fail

Agent unreachable → node stays healthy if `/api/tags` works; keep last
discovered capacity; set `capacity_error`; degrade to static / `ps_lower_bound`.
Never flip the node unhealthy solely because `:11436` is down.

## Merge

Effective capacity fills omitted static fields from discovery. Explicit static
VRAM/RAM **cap** discovered values. Trust the agent's `pressure_level` when
present; until then keep `Unknown`. Do not reclassify in the router.

`apply_capacity_report` also persists `vram_free_known`, used VRAM, mean GPU
util, `gpu_backend`, CPU%, loaded-model count, disk, and bounded per-GPU rows.
The binary re-exports these as `ollama_router_node_*` gauges (`{node}`;
`{node,gpu}` only for per-GPU VRAM). Grafana joins via `node_info`. Do not
label by model name. Do not extend `node_info` labels — backend lives on
`ollama_router_node_backend_info{node,backend}`.

Gate VRAM/util/RAM in PromQL on `*_known == 1`. A full GPU is free=0 with
known=1. Prometheus must not scrape production `:11436`.

## Tests

httpmock + fixtures matching the agent JSON. Assert `32 * 1024³` bytes → 32.0 GiB.
No live hosts required. Cover nvidia fallback (free unknown), full GPU
(free 0 known), ROCm CSV fixtures, Metal unknown VRAM, Windows pressure
without load, and router `refresh_gauges` known flags.
