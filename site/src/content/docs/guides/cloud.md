---
title: Cloud spots
description: Verda NVIDIA spot GPUs and RunPod interruptible pods — tunnel/loopback-only, idle-teardown owned by the router.
sidebar:
  order: 5
---

The router can burst onto cloud GPUs when the fleet is saturated. Two
providers are supported — **Verda** and **RunPod** — and no other.

## Shared model

- Cloud Ollama URLs are **tunnel/loopback-only**: a self-hosted zrok
  **private** share. A public `:11434` (or a hostname public tunnel like
  `*.zrok.io`) is `public_url_blocked` and **never healthy**. There is no
  public-proxy fallback.
- **No SSH.** Verda bootstraps via a startup script that installs the
  node-agent and spawns the zrok sidecar; RunPod uses a container
  `dockerStartCmd`. The router never opens an SSH session.
- Registration only after **enroll** succeeds and `/api/tags` responds through
  the tunnel.
- **Idle teardown is router-owned** and driven only by client inference
  forwards (native generate/chat/embed and OpenAI chat/completions,
  completions, embeddings). Health, `/api/ps`, capacity, admin, and the
  warm-keeper do **not** count. Fleet hosts are **never destroyed**.
- A capacity miss enqueues a coalesced async create — the client gets
  **503 + `Retry-After`**, never a blocked provision.
- One router replica: two processes sharing FleetState can double-create GPUs.

## Verda

OAuth2 **client-credentials** (`VERDA_*` env). The selector joins
`GET /v1/instance-availability` × `GET /v1/instance-types` and ranks **NVIDIA
spot** GPUs in the inclusive **8–80 GiB** VRAM window by cheapest price, then
smallest VRAM, then fewest GPUs. Instances are tagged
`managed_by=ollama-router`. Destroy uses `delete_permanently` — spot billing
only stops on delete.

## RunPod

Bearer REST (`RUNPOD_API_KEY` env; v1 pods + v2 GPU catalog). Best $/VRAM
selection within the hourly cap, `dockerStartCmd` bootstrap (never SSH), and
terminate-permanently teardown (never stop-only).

## Tunnel config

```bash
export ZROK_API_ENDPOINT=https://zrok.example.internal   # self-hosted controller
export ZROK_ENABLE_TOKEN=...                              # router zrok enable
export OLLAMA_ROUTER_ZROK_API_ENDPOINT=$ZROK_API_ENDPOINT # tunables can name the env
```

Tokens stay in process env and `.local/` files that are gitignored — never in
`fleet.yaml`, never logged, never in this documentation.
