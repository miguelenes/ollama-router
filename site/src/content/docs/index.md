---
title: ollama-router
description: Mixed CPU+GPU Ollama-compatible fleet proxy — one URL, many Ollama hosts.
hero:
  tagline: One URL, many Ollama hosts.
  image:
    file: ../../assets/banner.svg
    alt: ollama-router — one URL, many Ollama hosts
  actions:
    - text: Quick start
      link: /guides/quick-start/
      icon: right-arrow
      variant: primary
    - text: GitHub
      link: https://github.com/miguelenes/ollama-router
      icon: github
      variant: secondary
---

Clients speak ordinary Ollama to **one** listen URL. The router load-balances
generate, chat, and embed across a mixed CPU+GPU fleet you already run — the
router process itself needs **no GPU** — then optionally bursts onto Verda
NVIDIA **spot** GPUs or RunPod interruptible GPU pods over a self-hosted zrok
**private** share.

- **Ollama + OpenAI surface** — native `/api/*` plus OpenAI-compatible `/v1/*`,
  with an honest proxy contract (see [Architecture](/guides/architecture/)).
- **Utilization WLC** — utilization-first ranking with RAM pressure, GPU
  utilization, and request-class preference.
- **fleet.yaml GitOps** — permanent hosts in one file; cloud spots are dynamic
  and never listed there.
- **Node agent** — a small agent on every Ollama host reports capacity and
  pressure on `:11436`.
- **No secrets in docs** — every example below uses env placeholders.

## Where to go

- [Quick start](/guides/quick-start/) — run the router in minutes.
- [Installation](/guides/installation/) — router binary or Docker, plus the node agent.
- [fleet.yaml inventory](/guides/fleet/) — declare your permanent hosts.
- [Node agent](/guides/node-agent/) — capacity reporting on every host.
- [Cloud spots](/guides/cloud/) — Verda and RunPod GPU autoscale.
- [Status codes](/faq/status-codes/) — what every 503 reason means.
