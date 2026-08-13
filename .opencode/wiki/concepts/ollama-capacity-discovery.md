---
title: Ollama Capacity Discovery
tags: [ollama, router, capacity, ram-pressure, rust]
sourceRefs:
  - crates/ollama-router-core/src/capacity
  - crates/ollama-router-core/src/fleet
  - /home/menes/Projects/illumination/services/ollama-capacity-agent
lastReviewed: 2026-08-13
---

# Ollama Capacity Discovery

The router probes a **node-local** capacity agent on port `11436`. The
implementation lives in the sibling crate
`/home/menes/Projects/illumination/services/ollama-capacity-agent/`
(Axum 0.8, sysinfo, `nvidia-smi` subprocess). **Do not reimplement the agent
in this repo.** The router owns only the HTTP client and merge policy in
`crates/ollama-router-core/src/capacity/`.

Wire: `GET /healthz`, `GET /v1/capacity`, `GET /v1/pressure`. Optional bearer
`OLLAMA_CAPACITY_TOKEN` on `/v1/*` (`/healthz` always open). JSON field names
must stay compatible with the agent's `CapacityReport` / `Pressure` shapes.

**GiB = bytes / 1024³.** sysinfo reports RAM in bytes. Never divide by `1024²`
(that inflates GiB by 1024× and breaks absolute RAM thresholds).

Agent-down is **soft-fail**: node health still follows Ollama `/api/tags`, the
last discovered capacity is retained, `capacity_error` is populated, and routing
degrades to static / `ps_lower_bound` values.

Default probe URL: `http://{ollama-url-host}:11436/...`. Override per node with
fleet.yaml `capacity_url`.

## Effective capacity

The registry retains configured, discovered, and effective capacities. Effective
fills omitted static fields from discovery. Explicit static VRAM/RAM **cap**
discovered values; explicit GPU/core values **override**. When both are absent,
effective stays unknown. Positive loaded VRAM from `/api/ps` can only provide a
lower bound (`ps_lower_bound`). Admission uses effective capacity; live
loaded/reserved VRAM tightens request admission but does not make placement
transiently ineligible.

## RAM pressure

Each node tracks `pressure_level` (`ok | elevated | critical | unknown`).
Classification is worst-signal-wins (available-RAM ratio/GB, swap amplifier,
load-per-CPU). No live MemAvailable and no usable PS RAM → `unknown` (permissive).

The **router reclassifies** with its `PolicyConfig` thresholds. The agent's
`pressure_level` is an ops hint only.

Critical nodes hard-reject when `reject_on_ram_critical`. Elevated nodes reject
classes in `reject_on_ram_elevated_for_classes` (default medium/large). See
[[concepts/ollama-router-load-share]] for scoring penalties.
