---
title: fleet.yaml inventory
description: Permanent hosts live in a GitOps fleet.yaml; tunables overlays are tunables-only.
sidebar:
  order: 3
---

Fleet membership is **not** a tunables YAML list. Permanent hosts live in a
GitOps `fleet.yaml` pointed to by `OLLAMA_ROUTER_FLEET` (default
`/etc/ollama-router/fleet.yaml`).

| Source | Role |
| --- | --- |
| `fleet.yaml` | Permanent hosts (CPU and GPU). **Never destroyed on idle.** |
| FleetState | Durable enroll/tunnel URLs and cloud metadata |
| Verda / RunPod managers | Dynamic spot GPUs (never listed in fleet.yaml) |

## Tunables overlay

`OLLAMA_ROUTER_CONFIG` points at a thin YAML overlay (see
`config.overlay.example.yaml`) that overrides committed tunables. The overlay
is **tunables only**: a top-level `nodes:` key is a **hard config error**.

```yaml
# GOOD — tunables only; membership is fleet.yaml + FleetState + cloud managers
policy:
  default_max_inflight: 2

# BAD — inventory does not belong in the overlay
# nodes:
#   - id: local
#     url: http://127.0.0.1:11434
```

## Rules

- Fleet hosts are **never destroyed on idle** — teardown only ever targets
  cloud instances.
- Run **one router replica**. Two processes sharing one FleetState file can
  double-create cloud GPUs (the file lock is same-host only). Do not add Redis
  or HA replicas.
- Same-LAN fleet URLs stay direct HTTP. Cloud URLs are tunnel/loopback-only.
- Cloud credentials belong in process env, never in YAML.
- `PUT /router/v1/nodes` and enroll are debug/adopt paths: they update live
  state and may store a tunnel URL in FleetState, but they **never write
  fleet.yaml**.
- Capacity facts come from the node agent; `vram_gb: 0` with `gpus: 0` is a
  *measured CPU* — an omitted VRAM/GPU field is *unknown*, and the router never
  encodes unmeasured VRAM as `0`.
