---
title: Ollama Router Load-Share Model
tags: [ollama, router, routing, load-share, utilization, wlc]
sourceRefs:
  - crates/ollama-router-core/src/routing
  - crates/ollama-router-core/src/fleet
lastReviewed: 2026-08-16
---

# Ollama Router Load-Share Model

The router distributes requests proportionally to each node's effective
concurrency capacity (VRAM tier). The primary sort key is **utilization**
(`inflight / base_cap`), not absolute inflight count. Implementation:
`crates/ollama-router-core/src/routing/` (pure functions, no I/O).

## Sort key (lower is better)

| Position | Field | Description |
|---|---|---|
| 1 | Utilization × weight | `inflight / base_cap` × `inflight_weight` |
| 2 | Pressure penalty | RAM pressure: elevated +2, critical +8 |
| 3 | Known GPU util | Soft band (busy ≥ 50%); unknown util is middle, **not** `0` |
| 4 | Known free VRAM | Soft band (tight < 2 GiB); unknown free is middle, **not** `0` full |
| 5 | Capacity preference | Class-aware VRAM affinity (see below) |
| 6 | Warm score + RAM bias | 0 if loaded, 1 if cold; RAM available ratio tie-break |

`base_inflight_cap` is the concurrency ceiling **before** pressure derating:
explicit per-node `max_inflight`, fleet-wide `default_max_inflight`, or the
VRAM-tier suggestion. Pressure is a scoring penalty — it does not inflate the
utilization ratio. Inflight utilization **dominates** GPU-util and free-VRAM
bias and class preference. Metrics may still publish `gpu_util_pct=0` /
`vram_free_gb=0` when the matching `*_known` flag is false; ranking must not
treat those as idle or full.

## Class preference (`capacity_preference`)

Omitted VRAM/GPU count is **unknown**, not a measured CPU (`0` / `gpus: 0`).
Preference uses **known** values only; EMBED/MEDIUM must not sort omitted as `0`.

| Class | Preference | Intent |
|---|---|---|
| EMBED | known `vram_gb` (lower wins); unknown after known; +100 when free VRAM < 2 GiB | Reserve big GPUs; soft-penalize tight nodes |
| SMALL | known GPU, then unknown, then known CPU | GPU-first; CPU overflow |
| MEDIUM | known `vram_gb` (lower wins); unknown after known | Keep big GPUs free for LARGE |
| LARGE | `-known vram` (higher wins); unknown hard-gated out | Biggest GPU first |
| GENERIC | known `vram_gb` | Neutral |

MEDIUM/LARGE static gates require known sufficient VRAM (unknown does not fit).

Class preference steers *which* node within a similar utilization band receives
a request class. It never overrides a genuine utilization difference.

## Hard filters (before scoring)

1. Healthy
2. Label match (`must_have_labels` / `avoid_labels`)
3. Model present on disk (not necessarily loaded)
4. `capacity_fits` — static VRAM gate + live headroom + reservation ledger
5. Not saturated — `inflight < effective_max_inflight`

A saturated node is **hard-filtered**. All otherwise-eligible nodes saturated →
`all_nodes_saturated` → proxy 503.

## Concurrency tiers (`suggested_max_inflight`)

| VRAM | Default cap |
|---|---|
| < 12 GiB | 2 |
| 12 – 24 GiB | 3 |
| 24 – 48 GiB | 4 |
| ≥ 48 GiB | 8 |

Pressure derating: elevated −1 (floor 1); critical → 1. Explicit per-node
`max_inflight` and fleet `default_max_inflight` bypass VRAM-tier derating.

## Model classification

- Untagged models (no `:Nb` suffix) → MEDIUM
- MoE (e.g. `qwen3:30b-a3b`) parse **total** params (30B), not active (3B)
- Embedding markers (`embed`, `e5-`, `bge-`, `arctic-embed`) override size
- Known small bases (`moondream`, `minicpm-v`) → SMALL

LARGE is capacity-gated by the static VRAM estimate. Scoring cannot override it.

Sticky affinity is a final tie-break: promote the owner only when its complete
load key equals the best candidate. Retries re-rank remaining eligible nodes and
exclude attempted ids. See [[concepts/ollama-router-phase-3-retry-and-memory-safety]].
