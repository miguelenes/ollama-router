---
title: Troubleshooting
description: Common failure modes and how to read them.
sidebar:
  order: 2
---

## I get 503 model_missing

The model is not on any healthy node. The router never pulls on miss
(`auto_pull_on_miss` is false by default). Either place the model first via
`POST /router/v1/models/ensure`, or enable `auto_pull_on_miss` (placement-gated).

## A cloud node never becomes healthy

Cloud URLs must be **tunnel/loopback-only** (self-hosted zrok private share).
A public `:11434` or a hostname public tunnel (`*.zrok.io` etc.) is
`public_url_blocked`. Enroll the share via `POST /router/v1/nodes/enroll`, then
verify `/api/tags` through the tunnel.

## Ranking feels CPU-first on my fleet

Omitted `vram_gb`/`gpus` means *unknown*, not a measured CPU. Fill in real
capacity via the node agent (`:11436` probes) instead of guessing; unknown
sorts in the middle, not as zero.

## Admin API returns 403

`OLLAMA_ROUTER_ADMIN_TOKEN` is unset (fail-closed, no default) or the header
is missing. Export the token and send
`Authorization: Bearer ${OLLAMA_ROUTER_ADMIN_TOKEN}`.

## Two replicas double-created cloud GPUs

Run **one** router replica. The FleetState file lock is same-host only; two
processes sharing one state file can double-create. There is no Redis/HA mode.

## Grafana shows no nodes

Prometheus scrapes the router (`:11435` in the dev flow), not node-agent
`:11436`. Start `task dev` and check `up{job=...}` in Prometheus `:9090`.
