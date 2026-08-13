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

CPU-only Ollama-compatible fleet proxy. One URL (`:11434`) load-balances
embeddings and chat across env hosts and optional Verda Tailscale GPU nodes.

This crate is **not** Illumination. The Python tree at
`/home/menes/Projects/illumination/services/ollama-router/` is a behavioral
spec — read it; do not paste it.

## Inventory (env-first)

| Source | Role |
|--------|------|
| `OLLAMA_HOST_NN_*` | Primary static fleet membership |
| `FleetState` | Durable Tailscale URLs + Verda metadata |
| Verda manager | Dynamic spot GPUs |
| `OLLAMA_ROUTER_NODES` | Compact test/dev override only |

YAML overlays are **tunables-only**. Top-level `nodes:` is a hard config error.

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
[[concepts/ollama-router-durable-model-operations]].

## Dangerous flags

- `policy.unsafe_single_node_mutate` (default false) — single-node `/api/pull`
  and `/api/delete` passthrough. Prefer admin ensure/delete APIs.
- `/api/embeddings` → `/api/embed` is Ollama ≤0.32 protocol compatibility, not debt.

## Related

- [[concepts/ollama-cloud-vram-guardrails]]
- [[concepts/ollama-capacity-discovery]]
- [[concepts/ollama-router-durable-model-operations]]
- [[concepts/ollama-router-idle-scale-down]]
