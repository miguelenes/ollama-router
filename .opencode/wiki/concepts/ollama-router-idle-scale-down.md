---
title: Ollama Router Idle Scale-Down
tags: [ollama-router, verda, scale-down, idle, cost]
sourceRefs:
  - crates/ollama-router-core/src/cloud
  - crates/ollama-router-core/src/fleet
  - crates/ollama-router-verda
  - crates/ollama-router/src/proxy
lastReviewed: 2026-08-13
---

# Ollama Router Idle Scale-Down

Router-owned teardown of **Verda** spot GPUs when no proxied client traffic is
observed for `idle_timeout_seconds`. Implemented on the shared cloud manager in
`crates/ollama-router-core/src/cloud/`. Verda is the only provider.

## Activity signal

The proxy updates the per-node idle timestamp only inside
`Registry::inflight_inc` (atomic millis; `last_client_request_at` is the
reader) — called exclusively from client forward paths
(`generate`, `chat`, `embed` / embeddings). That write site is the **only**
activity signal for idle scale-down.

Does **not** count:

- Health probes (`set_healthy`)
- `/api/ps` polling
- Capacity-agent probes
- Admin `/router/v1/*`
- Warm-keeper generate probes
- Internal reconcile

## Idle policy

Runs inside `reconcile()`, gated on `auto_scale` and `idle_scale_down_enabled`
(both default on).

Eligibility (all must hold):

1. Instance is owned (never a **fleet.yaml** permanent host)
2. `inflight == 0`
3. `now - registered_at >= idle_grace_after_create_seconds`
4. `now - activity_anchor >= idle_timeout_seconds`

Activity anchor: `last_client_request_at` when the proxy has forwarded a client
request; otherwise `registered_at`. A router **restart does not mass-destroy**
cloud GPUs.

Candidates sort longest-idle first. Never destroy below `auto_scale_min_instances`.
Verda destroy uses `delete_permanently`. Failed destroy **retains** Registry and
FleetState ownership so reconcile can retry the still-billed resource.

## Demand-driven scale-up

When idle teardown leaves no healthy capacity, the next client miss
(`no_healthy_nodes` / `no_nodes_configured` / `all_nodes_saturated` /
`insufficient_capacity`) triggers coalesced async Verda **`create_additional`**
(never `ensure`, never blocks the client). The client receives
**503 + `Retry-After: 30`**. Cold create may take minutes (provision + model pull).

Demand skips when `auto_scale` is false or owned `verda-*` count is already at
`auto_scale_max_instances` (`0` means unlimited).

`create_additional` is scale-out: it must not adopt an existing running resource.
`ensure(create=true)` stays adopt-first for startup and admin.

## Shutdown

`destroy_on_shutdown` defaults true. Skip destroy when process uptime is still
inside `idle_grace_after_create_seconds` so a crash-loop cannot mass-delete spots.

## v1 replica limit

One router process per FleetState file. Two replicas sharing the same state path
can double-create spots. The file lock is same-host only. Do not add Redis.

## Config knobs (Verda)

| Knob | Default | Env |
|------|---------|-----|
| `idle_scale_down_enabled` | `true` | `VERDA_IDLE_SCALE_DOWN_ENABLED` |
| `idle_timeout_seconds` | `900` | `VERDA_IDLE_TIMEOUT_SECONDS` |
| `idle_grace_after_create_seconds` | `300` | `VERDA_IDLE_GRACE_AFTER_CREATE_SECONDS` |
| `auto_scale_min_instances` | `0` | `VERDA_AUTO_SCALE_MIN_INSTANCES` |
| `auto_scale_max_instances` | `2` | `VERDA_AUTO_SCALE_MAX_INSTANCES` |
| `auto_scale` | `true` | `VERDA_AUTO_SCALE` |

Day-1: `auto_scale_min_instances=0`, idle 900s, enable Verda only when needed.
Do **not** install node-local cron/systemd idle killers — health probes fake
activity, and nodes must not hold provider API credentials.

## Related

- [[concepts/ollama-router-product]]
- [[concepts/ollama-cloud-vram-guardrails]]
