---
name: routing-wlc
description: Implements utilization-aware weighted least-connections routing with class preference — hard filters, sort key, EMBED/SMALL/MEDIUM/LARGE/GENERIC table, sticky affinity. Use when changing node selection, saturation, VRAM admission, or rank_nodes tests.
---

# Routing WLC

Read `.opencode/wiki/concepts/ollama-router-load-share.md`.
Code: `crates/ollama-router-core/src/routing/` (pure functions, unit + proptest).

## Hard filters (drop, do not score)

1. Healthy
2. Label match
3. Model present on disk
4. `capacity_fits` (static VRAM + live headroom + reservations)
5. Not saturated (`inflight < effective_max_inflight`)

All eligible saturated → `all_nodes_saturated` → 503. Never pick a saturated node
as fallback.

## Sort key (lower is better)

1. `(inflight / base_cap) * inflight_weight`
2. Pressure penalty: elevated +2, critical +8
3. `capacity_preference` (class bias)
4. Warm (loaded 0 / cold 1) + RAM-available-ratio tie-break

`base_cap` is the ceiling **before** pressure derating. Utilization **dominates**
preference: a 48 GiB GPU at 2/8 (25%) beats a CPU at 1/2 (50%) for EMBED.

## Class preference

| Class | Preference |
|-------|------------|
| EMBED | lower `vram_gb`; +100 if free VRAM < 2 GiB |
| SMALL | GPU `vram_gb`; CPU `vram_gb + 100` |
| MEDIUM | lower `vram_gb` |
| LARGE | higher VRAM (`-vram_gb`) |
| GENERIC | `vram_gb` |

LARGE is hard-gated by the static VRAM estimate (q4 rule of thumb).

Sticky affinity: promote owner only when its **full load key equals** the best
candidate. Retries re-rank and exclude attempted node ids.

## Tests

proptest: utilization difference beats preference bias; LARGE never lands on
undersized VRAM; saturated nodes never selected.
