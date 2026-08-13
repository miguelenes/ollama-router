---
name: verda-spot-fleet
description: Implements Verda Cloud NVIDIA spot GPU fleet management — OAuth2 client-credentials, availability×types join, cheapest/smallest GPU in 8–80 GiB, SSH bootstrap to ordinary OpenSSH over Tailscale, and delete_permanently. Use when working under crates/ollama-router-verda or cloud provision/reconcile.
---

# Verda spot fleet

Read `.opencode/wiki/concepts/ollama-cloud-vram-guardrails.md` and
`.opencode/wiki/concepts/ollama-router-product.md`. Code:
`crates/ollama-router-verda/`.

Verda is the **only** cloud provider. Do not add Thunder or RunPod.

## Auth

OAuth2 client-credentials against Verda (`/v1/oauth2/token`).

- Pre-refresh with ~30s leeway.
- One retry on 401 after refresh.
- Honor 429 `Retry-After` (cap wait, e.g. 60s).
- Never log access tokens, refresh tokens, or client secrets.

## Selector (pure, unit-tested)

Join `GET /v1/instance-availability` with `GET /v1/instance-types`.

Keep:

- NVIDIA GPU (reject CPU-only and non-NVIDIA)
- Spot price present (never on-demand as the rank key)
- Inclusive VRAM window default **8–80 GiB** (`VERDA_MIN_VRAM_GB` / `VERDA_MAX_VRAM_GB`)
- Type allow/deny globs, location allowlist, optional `max_spot_price_per_hour`

Rank: cheapest spot → smallest VRAM → fewest GPUs → location preference.
Never default to huge SKUs. DTOs: serde **ignore extra fields**.

## Provision URL

1. Create instance (spot). OS volume `on_spot_discontinue=delete_permanently`.
2. Public SSH bootstrap only until Tailscale is up.
3. Switch to **ordinary OpenSSH** at the Tailscale IP (not Tailscale SSH).
4. Register Ollama URL only after OpenSSH + `GET /api/tags` succeed on the tailnet.
5. Public `:11434` is `public_url_blocked` — never a routing URL.

## Destroy

`delete_permanently=true`. Spot billing stops only on delete. Teardown failure
retains FleetState ownership.

## Tests

httpmock only. **No live Verda.** proptest the ranking key.
