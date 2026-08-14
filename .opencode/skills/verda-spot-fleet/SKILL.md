---
name: verda-spot-fleet
description: Implements Verda Cloud NVIDIA spot GPU fleet management — OAuth2 client-credentials, availability×types join, cheapest/smallest GPU in 8–80 GiB, startup-script bootstrap (never SSH) plus self-hosted zrok private share, and delete_permanently. Use when working under crates/ollama-router-verda or cloud provision/reconcile.
---

# Verda spot fleet

Read `.opencode/wiki/concepts/ollama-cloud-vram-guardrails.md`,
`.opencode/wiki/concepts/ollama-router-product.md`, and
`.opencode/wiki/concepts/ollama-router-node-tunnel.md`. Code:
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
   Preferred images stay Ubuntu 24 CUDA (`*ubuntu-24*cuda*docker*` first).
2. Bootstrap is a Verda **startup script** (`POST /v1/scripts`, attach
   `startup_script_id` on `POST /v1/instances`). Reuse by name
   (`ollama-router-agent-init`) or a configured id. The catalog body downloads
   the matching agent `.deb`/tarball (or `verda.agent_package_url`), installs
   it, writes enroll URL + tokens from a 0600 env file, and runs
   `ollama-node-agent setup` (elevated). Tokens are interpolated from router-side
   env names (`ZROK_ENABLE_TOKEN`, `OLLAMA_ROUTER_ADMIN_TOKEN`) at create — never
   committed. `tunnel.api_endpoint` is injected as `ZROK_API_ENDPOINT`. The
   script must not echo secrets, must not install Tailscale, and must not curl
   public `:11434`.
3. The Verda API does **not** take inline `startup_script` on instance create
   (SDK field is `startup_script_id` only). DTOs serde-ignore extras; never
   `deny_unknown_fields` on Verda JSON.
4. The router may still upload an SSH public key to satisfy Verda's API. It
   must **never SSH**. Do not add russh, OpenSSH, or Tailscale handoff. Do not
   wait on public SSH.
5. After `status=running`, wait/reconcile polls FleetState enroll for that
   instance id (`verda-{instance_id}`), then `GET {tunnel}/api/tags`. Timeout
   uses an allowlisted reason (`enroll_timeout`) and **keeps** FleetState
   ownership. Do not persist provider error text in SQLite. Guest heartbeat may
   send the create hostname as `id`; enroll maps it onto the Verda row.
6. Register the Ollama URL only after enroll of the zrok **private** share
   token and `GET /api/tags` succeed through the tunnel. On the instance,
   Ollama and the node-agent bind **loopback**; the share is the only ingress.
7. Public `:11434` is `public_url_blocked` — never a routing URL. Hostname
   public tunnels (`*.zrok.io` etc.) are also rejected.

Logs on create: instance type, location, VRAM only — never script bodies,
enable tokens, or admin bearers.

Tag created instances `managed_by=ollama-router` (not an Illumination prefix).
FleetState key `managed_by=verda` stays the ownership discriminator. Default
`ssh_key_name` is `ollama-router` (API requirement only). Enroll must not write
`fleet.yaml`. Adopt-first `ensure` must not recreate a running owned instance.

## Destroy

`delete_permanently=true`. Spot billing stops only on delete. Teardown failure
retains FleetState ownership.

## Tests

httpmock only. **No live Verda.** Instance create POST must include
`startup_script_id`. Adopt must not POST `/v1/instances`. No
`wait_for_public_ssh`. proptest the ranking key.
