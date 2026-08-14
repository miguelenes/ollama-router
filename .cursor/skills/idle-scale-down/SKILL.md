---
name: idle-scale-down
description: Implements router-owned Verda idle teardown driven only by last_client_request_at from inflight_inc on generate/chat/embed and OpenAI /v1 inference. Use when changing registry inflight, health probes, warm-keeper, cloud reconcile, or idle timeout knobs.
---

# Idle scale-down

Read `.opencode/wiki/concepts/ollama-router-idle-scale-down.md`.
Implementation: `crates/ollama-router-core/src/cloud/` + `Registry::inflight_inc`.

## Single write site

`last_client_request_at` updates **only** inside `inflight_inc`, and
`inflight_inc` is called **only** from proxied client inference forwards
(native generate/chat/embed **and** OpenAI `/v1/chat/completions`,
`/v1/completions`, `/v1/embeddings`). POST only.

| Counts | Does not count |
|--------|----------------|
| Client generate/chat/embed forwards | Health `set_healthy` |
| OpenAI chat/completions/embeddings | `/api/ps` |
| | Capacity agent |
| | Admin `/router/v1/*` |
| | Warm-keeper (occupies inflight without `inflight_inc`) |

## Eligibility

Owned Verda instance, `inflight == 0`, past create grace, past idle timeout.
Activity anchor = `last_client_request_at` or else `registered_at` (restart
must not mass-destroy). Never destroy **fleet.yaml** hosts. Never go
below `auto_scale_min_instances`.

Destroy with `delete_permanently`. Failed destroy keeps ownership.

## Demand scale-up

Client miss → coalesced async Verda `create_additional` → **503 + Retry-After**.
Do not block the request on provision. `create_additional` must not adopt
existing capacity. `ensure` stays adopt-first for startup and admin.

## Knobs

`VERDA_IDLE_SCALE_DOWN_ENABLED`, `VERDA_IDLE_TIMEOUT_SECONDS` (default 900),
`VERDA_IDLE_GRACE_AFTER_CREATE_SECONDS` (default 300), min/max instances.

Do not install node-local cron/systemd idle killers.
