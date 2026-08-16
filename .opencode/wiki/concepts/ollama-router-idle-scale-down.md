---
title: Ollama Router Idle Scale-Down
tags: [ollama-router, verda, runpod, scale-down, idle, cost]
sourceRefs:
  - crates/ollama-router-core/src/cloud
  - crates/ollama-router-core/src/fleet
  - crates/ollama-router-verda
  - crates/ollama-router-runpod
  - crates/ollama-router/src/proxy
lastReviewed: 2026-08-16
---

# Ollama Router Idle Scale-Down

Router-owned teardown of **cloud** GPUs (Verda spots and RunPod interruptible
pods) when no proxied client traffic is observed for `idle_timeout_seconds`.
Implemented as shared policy in `crates/ollama-router-core/src/cloud/`; each
provider manager executes destroy for its own origin. Thunder stays forbidden.

## Activity signal

The proxy updates the per-node idle timestamp only inside
`Registry::inflight_inc` (atomic millis; `last_client_request_at` is the
reader) — called exclusively from client inference forwards
(`POST /api/generate`, `/api/chat`, `/api/embed` / embeddings, and OpenAI
`POST /v1/chat/completions`, `/v1/completions`, `/v1/embeddings`). That write
site is the **only** activity signal for idle scale-down.

Does **not** count:

- Health probes (`set_healthy`)
- `/api/ps` polling
- Capacity-agent probes
- Admin `/router/v1/*`
- Warm-keeper generate probes
- Internal reconcile
- Fleet unload / `ollama stop` (unload-intent generate/chat: no `inflight_inc`)

## Idle policy

Runs inside each manager's `reconcile()`, gated on that provider's `auto_scale`
and `idle_scale_down_enabled` (both default on).

Eligibility (all must hold):

1. Instance is owned by this provider (never a **fleet.yaml** permanent host)
2. `inflight == 0`
3. `now - registered_at >= idle_grace_after_create_seconds`
4. `now - registered_at >= min_lifetime` (per-provider `min_lifetime_seconds`; default `0`)
5. `now - activity_anchor >= idle_timeout_seconds`

Activity anchor: `last_client_request_at` when the proxy has forwarded a client
request; otherwise `registered_at`. A router **restart does not mass-destroy**
cloud GPUs.

Candidates sort longest-idle first. Never destroy below `auto_scale_min_instances`.

Before destroy, the manager marks the node **draining** (ranking skips it;
`Registry::inflight_inc` refuses new client forwards so the proxy retries another
node before the first byte). It then re-reads `inflight` and the idle timestamps.
If inflight is non-zero or the node is no longer idle, it **undrains** and skips.
Only then does Verda destroy use `delete_permanently` and RunPod **terminate**
the pod (never stop-only). Failed destroy **keeps draining** and **retains**
Registry and FleetState ownership so reconcile can retry the still-billed
resource.

Operator cordon (`POST /router/v1/nodes/{id}/drain`) is a separate **cordoned**
bit — not inventory/cloud `draining`. It excludes the node from ranking but
never makes a fleet.yaml host destroyable and is not the idle teardown path.

Trimming above `auto_scale_max_instances` uses the same drain-and-verify path.
Victim order is lowest activity (`last_client_request_at` else `registered_at`),
then lowest inflight — never list order, never a fleet.yaml host.

## Demand-driven scale-up

When idle teardown leaves no healthy capacity, the next client miss
(`no_healthy_nodes` / `no_nodes_configured` / `all_nodes_saturated` /
`insufficient_capacity`) triggers coalesced async **`create_additional`** on the
best-value eligible provider via `MultiProviderDemand` (never `ensure`, never
blocks the client). The client receives **503 + `Retry-After: 30`**. Cold create
may take minutes (provision + model pull).

Demand skips when that provider's `auto_scale` is false or owned count is
already at `auto_scale_max_instances` (`0` means unlimited). `create_additional`
also enforces that cap under `ensure_lock` against a complete owned instance
list (plus registry ids as a floor).

`create_additional` is scale-out: it must not adopt an existing running resource.
`ensure(create=true)` stays adopt-first for startup and admin.

## Shutdown

`destroy_on_shutdown` defaults true per provider. Skip destroy when process
uptime is still inside `idle_grace_after_create_seconds` so a crash-loop cannot
mass-delete cloud GPUs.

## v1 replica limit

One router process per FleetState file. Two replicas sharing the same state path
can double-create spots/pods. The file lock is same-host only. Do not add Redis.

## Config knobs

Each provider (`verda:` / `runpod:`) mirrors the same envelope. Verda env
prefix `VERDA_*`; RunPod uses `RUNPOD_*` where wired.

| Knob | Default | Notes |
|------|---------|-------|
| `idle_scale_down_enabled` | `true` | Per provider |
| `idle_timeout_seconds` | `900` | Per provider |
| `idle_grace_after_create_seconds` | `300` | Per provider |
| `min_lifetime_seconds` | `0` | Billing floor; RunPod per-second → keep 0 |
| `auto_scale_min_instances` | `0` | Per provider |
| `auto_scale_max_instances` | `2` | Per provider |
| `auto_scale` | `true` | Per provider |
| `orphan_reclaim_enabled` | `true` | Per provider |
| `orphan_reclaim_grace_seconds` | `1800` | Per provider |

Day-1: floors at `0`, idle 900s, enable each provider only when needed.
Do **not** install node-local cron/systemd idle killers — health probes fake
activity, and nodes must not hold provider API credentials.

## Related

- [[concepts/ollama-router-product]]
- [[concepts/ollama-cloud-vram-guardrails]]
