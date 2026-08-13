---
title: Ollama node agent (capacity discovery)
tags: [ollama, router, capacity, ram-pressure, rust, node-agent]
sourceRefs:
  - crates/ollama-node-agent
  - crates/ollama-capacity-types
  - crates/ollama-router-core/src/capacity
  - crates/ollama-router-core/src/fleet
lastReviewed: 2026-08-13
---

# Ollama node agent

Every Ollama host in the mixed CPU/GPU fleet runs **`ollama-node-agent`**
(`crates/ollama-node-agent`). The router process needs **no GPU** and does not
install Ollama. They meet over HTTP on port `11436`.

Privilege split:

- `ollama-node-agent setup` — elevated, idempotent converge (install + OS service)
- `ollama-node-agent serve` — unprivileged HTTP on `:11436`
- `ollama-node-agent doctor` — no side effects
- `ollama-node-agent uninstall` — best-effort unit/plist/task removal

The agent never talks to Verda and never owns cloud idle teardown. Tailscale
join is setup-only when `tailscale.enable` and an auth key are present; the
serve process must not hold `TS_AUTHKEY` / `VERDA_*`.

Shared JSON types live in `crates/ollama-capacity-types`. The router owns only
the HTTP client and merge policy in `crates/ollama-router-core/src/capacity/`.

Wire: `GET /healthz`, `GET /metrics` (open); `GET /v1/capacity`, `/v1/pressure`,
`/v1/status` (bearer if `token` / `OLLAMA_NODE_AGENT_TOKEN` is set). JSON field
names stay compatible with the historical Illumination capacity agent;
additive keys only (`gpu_backend` on capacity/status).

**GiB = bytes / 1024³.** sysinfo reports RAM in bytes. Never divide by `1024²`.
nvidia-smi memory is MiB (`/ 1024` → GiB).

Agent-down is **soft-fail**: node health still follows Ollama `/api/tags`, the
last discovered capacity is retained, `capacity_error` is populated, and routing
degrades to static / `ps_lower_bound` values.

Default probe URL: `http://{ollama-url-host}:11436/...`. Override per node with
fleet.yaml `capacity_url`.

`GET /v1/status` is how a later router slice can learn `gpu_backend`
(`cpu|cuda|rocm|metal|unknown`) without guessing labels. Metal reports
`gpu_backend=metal` and optional `metal_recommended_gb`; it does **not** fake
CUDA VRAM (`vram_gb=0` on Apple unless a real discrete inventory exists).

Remote provision (later): upload the agent binary and run `setup`. Do not keep
embedding Ubuntu Ollama install logic in `provision-ollama-gpu.sh` once that
handoff exists.

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
**The agent classifies** (worst-signal-wins: available-RAM ratio/GB, swap
amplifier, load-per-CPU). The router **trusts** the wire token via
`PressureLevel::from_wire`. Do not port `classify_pressure` knobs into router
`PolicyConfig`.

No live MemAvailable and no usable load → `unknown` (permissive).

Critical nodes hard-reject when `reject_on_ram_critical`. Elevated nodes reject
classes in `reject_on_ram_elevated_for_classes` (default medium/large). See
[[concepts/ollama-router-load-share]] for scoring penalties.

Optional `register` heartbeat to the router is **off by default**. Production
membership is fleet.yaml. Heartbeat must be authenticated and must not let a
random laptop join a production router.
