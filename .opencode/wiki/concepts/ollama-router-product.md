---
title: ollama-router product surface
tags: [ollama-router, fleet, verda, embeddings]
sourceRefs:
  - AGENTS.md
  - crates/ollama-router-core/src/config
  - crates/ollama-router/src/proxy
  - crates/ollama-router-verda
  - crates/ollama-router-core/src/jobs
lastReviewed: 2026-08-13
---

# ollama-router product surface

Mixed CPU+GPU Ollama-compatible fleet proxy. The router process needs **no GPU**.
One URL (`:11434`) load-balances embeddings and chat across `fleet.yaml` hosts
and optional Verda Tailscale GPU nodes.

This crate is **not** Illumination. The Python tree at
`/home/menes/Projects/illumination/services/ollama-router/` is a behavioral
spec — read it; do not paste it.

## Inventory (fleet.yaml)

| Source | Role |
|--------|------|
| `OLLAMA_ROUTER_FLEET` / `fleet.yaml` | Permanent CPU and GPU membership |
| `FleetState` | Durable Tailscale URLs + Verda metadata |
| Verda manager | Dynamic spot GPUs (not in fleet.yaml) |

YAML overlays are **tunables-only**. Top-level `nodes:` is a hard config error
(wrong file). Never destroy fleet.yaml hosts.

Config lives under `crates/ollama-router-core/src/config/`: models + knobs +
layered load that rejects YAML inventory.

## Cloud

**Verda only.** NVIDIA spots, cheapest then smallest qualifying GPU inside
inclusive `min_vram_gb` / `max_vram_gb` (default 8–80). Never advertise public
`:11434` as healthy.

Path: public SSH bootstrap → ordinary OpenSSH over Tailscale. The provisioner
does **not** enable Tailscale SSH. Register an Ollama URL only after OpenSSH
and `/api/tags` succeed on the tailnet. Public SSH is bootstrap/recovery-only.

Demand scale-up calls the Verda manager only (no multi-provider ranking). It is
coalesced and asynchronous; the client receives **503 + Retry-After**.

`CloudFleetManager` reconcile (in `crates/ollama-router-core/src/cloud/`): list →
cleanup gone/terminal/evicted → orphan adopt → auto_scale up/down. Verda supplies
the hooks.

## Capacity

Production: sibling Rust `ollama-capacity-agent` on `:11436`. See
[[concepts/ollama-capacity-discovery]]. Do not reimplement the agent in this repo.

## Model operations

Pull/delete metadata persists to SQLite and recovers via live `/api/tags`; see
[[concepts/ollama-router-durable-model-operations]]. `POST /api/pull` and
`/api/delete` always go through the fleet orchestrator (stub → 503 until jobs
land). There is no single-node mutate passthrough.

`/api/embeddings` → `/api/embed` is Ollama ≤0.32 protocol compatibility, not debt.

Cloud instance tag `managed_by=ollama-router`. FleetState `managed_by=verda`
is an ownership discriminator, not the cloud tag.

## Related

- [[concepts/ollama-cloud-vram-guardrails]]
- [[concepts/ollama-capacity-discovery]]
- [[concepts/ollama-router-durable-model-operations]]
- [[concepts/ollama-router-idle-scale-down]]
