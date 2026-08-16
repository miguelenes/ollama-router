---
title: ollama-router product surface
tags: [ollama-router, fleet, verda, embeddings]
sourceRefs:
  - AGENTS.md
  - crates/ollama-router-core/src/config
  - crates/ollama-router/src/proxy
  - crates/ollama-router-verda
  - crates/ollama-router-core/src/jobs
lastReviewed: 2026-08-16
---

# ollama-router product surface

Mixed CPU+GPU Ollama-compatible fleet proxy. The router process needs **no GPU**.
One URL (`:11434`) load-balances embeddings and chat across `fleet.yaml` hosts
and optional Verda GPU nodes over a self-hosted zrok **private** share.

This crate is **not** Illumination. The Python tree at
`/home/menes/Projects/illumination/services/ollama-router/` is a behavioral
spec — read it; do not paste it.

## Inventory (fleet.yaml)

| Source | Role |
|--------|------|
| `OLLAMA_ROUTER_FLEET` / `fleet.yaml` | Permanent CPU and GPU membership (LAN URLs are direct HTTP) |
| `FleetState` | Durable enroll/tunnel URLs + Verda metadata |
| Verda manager | Dynamic spot GPUs (not in fleet.yaml) |

YAML overlays are **tunables-only**. Top-level `nodes:` is a hard config error
(wrong file). Never destroy fleet.yaml hosts.

Config lives under `crates/ollama-router-core/src/config/`: models + knobs +
layered load that rejects YAML inventory.

## Cloud

**Verda only.** NVIDIA spots, cheapest then smallest qualifying GPU inside
inclusive `min_vram_gb` / `max_vram_gb` (default 8–80). Never advertise public
`:11434` as healthy. Hostname public tunnels (`*.zrok.io` etc.) are also
`public_url_blocked`.

Path: Verda **startup script** (`startup_script_id` on instance create) installs
`ollama-node-agent` and runs `setup` (Ollama + zrok sidecar). Reuse catalog
script `ollama-router-agent-init` by name (or a configured id). Secrets are
injected from router env at create into a 0600 guest env file — never committed,
never echoed. `tunnel.api_endpoint` is also written as `ZROK_API_ENDPOINT` so
the guest `zrok enable` talks to the self-hosted controller. The router may upload an SSH key to satisfy Verda's API but must
**never SSH** (no russh, no public SSH wait). Register an Ollama URL only after
enroll of the private share token and `/api/tags` succeed through the tunnel.
Enroll timeout keeps FleetState ownership and returns an allowlisted reason.
On tunneled hosts, Ollama and the node-agent bind **loopback**.
`fleet.yaml` LAN URLs stay direct HTTP. Enroll must not write `fleet.yaml`.
Preferred images stay Ubuntu 24 CUDA. See [[concepts/ollama-router-node-tunnel]].

Demand scale-up calls the Verda manager only (no multi-provider ranking). It is
coalesced and asynchronous; the client receives **503 + Retry-After**.

`CloudFleetManager` reconcile (in `crates/ollama-router-core/src/cloud/`): list →
cleanup gone/terminal/evicted → orphan adopt → auto_scale up/down. Verda supplies
the hooks.

## Capacity

Production: `crates/ollama-node-agent` on `:11436` (`setup` elevated, `serve`
unprivileged). Shared DTOs in `crates/ollama-capacity-types`. See
[[concepts/ollama-capacity-discovery]]. The router owns only the HTTP client
and merge policy; it does not install Ollama or reclassify `pressure_level`.

## Model operations

This product is an **honest fleet proxy**, not a fake single daemon:

| Client action | Behavior |
|---------------|----------|
| List (`/api/tags`, `/v1/models`) | Union of healthy holders |
| Infer (generate/chat/embed + OpenAI) | Rank among nodes that **already have** the model (holders-only WLC) |
| Pull | Fleet **placement job** (not native NDJSON pull through one node) |
| Miss | **503** `model_missing` (not native Hub 404/pull); `auto_pull_on_miss` default **false** |
| create/copy/push/blobs | **501** `not_a_fleet_operation` |

Pull/delete metadata persists to SQLite and recovers via live `/api/tags`; see
[[concepts/ollama-router-durable-model-operations]]. `POST /api/pull` and
`/api/delete` always go through the fleet orchestrator. Default placement
targets every **healthy** node that fits the model's **generate** size class
(static VRAM). LARGE/MEDIUM skip known CPUs and **unknown** VRAM. There is no
single-node mutate passthrough.

**Capacity honesty:** omitted `capacity.vram_gb` / `gpus` is **unknown**, not a
measured CPU. YAML `0` / `gpus: 0` is a known CPU. MEDIUM and LARGE inference
and placement require known sufficient VRAM; unknown fails those gates
(`insufficient_capacity`). EMBED/SMALL/GENERIC may still use unknown holders.

`/api/embeddings` → `/api/embed` is Ollama ≤0.32 protocol compatibility, not debt.

`GET /api/tags` is a **CLI-compatible union** of healthy, non-draining nodes
(not names-only). Each row includes `name`, `model`, and a `digest` of at least
12 characters so `ollama list` can slice it. The digest is the probe value when
present; otherwise a stable SHA-256 hex of the normalized name. Probe `size`,
`modified_at`, native `details`, and `capabilities` are forwarded when known.
`details.router_nodes` lists every healthy holder. Listing is not inflight or
idle activity.

OpenAI `POST /v1/chat/completions`, `/v1/completions`, and `/v1/embeddings` are
passthrough to the ranked node's Ollama shim (same idle / reservation / class
ranking as native inference). Size class may use catalog `details.parameter_size`
when the name has no `:Nb` tag. `GET /v1/models` and `GET /v1/models/{id}` use the
same union; `created` is Unix seconds from the winning `modified_at` when
parseable, else `0`. Unknown `/v1/*` is 404. `POST /api/push`, `/api/copy`,
`/api/create`, and `/api/blobs*` are rejected (`not_a_fleet_operation`).

Cloud instance tag `managed_by=ollama-router`. FleetState `managed_by=verda`
is an ownership discriminator, not the cloud tag.

## Related

- [[concepts/ollama-cloud-vram-guardrails]]
- [[concepts/ollama-capacity-discovery]]
- [[concepts/ollama-router-node-tunnel]]
- [[concepts/ollama-router-durable-model-operations]]
- [[concepts/ollama-router-idle-scale-down]]
