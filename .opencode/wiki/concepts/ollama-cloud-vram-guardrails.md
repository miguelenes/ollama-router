---
title: Ollama cloud VRAM guardrails
tags: [ollama, router, verda, gpu, vram, cost-control]
sourceRefs:
  - crates/ollama-router-core/src/config
  - crates/ollama-router-verda
lastReviewed: 2026-08-13
---

# Ollama cloud VRAM guardrails

Verda GPU selection applies inclusive `min_vram_gb` / `max_vram_gb` **before**
ranking eligible candidates by price. Default window is **8–80 GiB**: ordinary
consumer/pro GPUs in, H200-class ~141 GiB cards out.

Env: `VERDA_MIN_VRAM_GB` / `VERDA_MAX_VRAM_GB`.

`max_vram_gb: null` removes the upper guardrail. Validation rejects negatives
and a maximum below the minimum.

These bounds constrain **new Verda GPU selection only**. They do not alter
capacity discovered from an existing node.

See [[concepts/ollama-capacity-discovery]] for node-level discovered capacity
and routing admission after a node has joined the fleet.
