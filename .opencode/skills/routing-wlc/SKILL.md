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
3. Model present on disk (**holders only** for inference)
4. `capacity_fits` (static VRAM + live headroom + reservations)
5. Not saturated (`inflight < effective_max_inflight`)

All eligible saturated → `all_nodes_saturated` → 503. Never pick a saturated node
as fallback.

A generate/chat/embed miss (`model_missing`) is not a rank bypass: the model must
be on disk before scoring. Optional proxy `auto_pull_on_miss` enqueues a
placement-gated fleet pull, then the next rank still applies these filters.

## Known vs unknown capacity

Omitted `vram_gb` / `gpus` is **unknown**, not a measured CPU. YAML `0` /
`gpus: 0` is a known CPU. Use `known_vram_gb()` / `known_gpus()` on the ranking
path (do not treat metrics `vram_gb()` → 0 as “CPU” for gates or preference).

- MEDIUM / LARGE: require `Some(vram)` meeting the static thresholds; unknown
  → `insufficient_capacity` (inference) or skip (placement).
- EMBED / SMALL / GENERIC: still admissible on unknown when other filters pass.
- Unknown may share the lowest-tier numeric inflight cap; that coincidence must
  not imply a measured CPU for SMALL preference or MEDIUM/LARGE gates.

## Sort key (lower is better)

1. `(inflight / base_cap) * inflight_weight`
2. Pressure penalty: elevated +2, critical +8
3. Known GPU util (busy band ≥ 50%; unknown is middle, **not** `0`)
4. Known free VRAM (tight band < 2 GiB; unknown is middle, **not** `0` full)
5. `capacity_preference` (class bias over **known** VRAM/GPU)
6. Warm (loaded 0 / cold 1) + RAM-available-ratio tie-break

`base_cap` is the ceiling **before** pressure derating. Utilization **dominates**
GPU-util / free-VRAM bias and preference: a 48 GiB GPU at 2/8 (25%) beats a CPU
at 1/2 (50%) for EMBED. Do not treat metrics `gpu_util_pct.unwrap_or(0)` or
`vram_free_gb.unwrap_or(0)` as idle/full on the rank path.

## Class preference

| Class | Preference |
|-------|------------|
| EMBED | lower **known** VRAM; unknown sorts after known; +100 if free VRAM < 2 GiB |
| SMALL | known GPU (`gpus >= 1`), then unknown, then known CPU (`gpus = 0`) |
| MEDIUM | lower **known** VRAM (omitted must not sort as `0`) |
| LARGE | higher known VRAM (`-vram`); unknown already hard-gated out |
| GENERIC | known VRAM (unknown after known) |

LARGE is hard-gated by the static VRAM estimate (q4 rule of thumb).

Sticky affinity: promote owner only when its **full load key equals** the best
candidate. Retries re-rank and exclude attempted node ids.

## Placement

Default pull/ensure uses generate-class gates (`placement_class`), not
`RequestClass::Pull`. Targets every healthy label-ok node that
`static_capacity_fits`. `#all` may widen; capacity / known-disk skips still
apply at run. Warm-keeper stays on-disk only; tier pick and free-VRAM skips use
**known** values (omitted MUST NOT encode as `0`).

## Tests

proptest: utilization difference beats preference bias; LARGE never lands on
undersized/unknown VRAM; saturated nodes never selected; SMALL prefers known
GPU over unknown over CPU.
