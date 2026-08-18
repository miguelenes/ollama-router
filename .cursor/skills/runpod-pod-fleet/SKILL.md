---
name: runpod-pod-fleet
description: Implements RunPod interruptible GPU pod fleet management — bearer REST (v1 pods + v2 GPU catalog), best $/VRAM selection with hourly cap, dockerEntrypoint + dockerStartCmd bootstrap (never SSH) plus self-hosted zrok private share, and terminate-permanently teardown. Use when working under crates/ollama-router-runpod or multi-provider demand.
---

# RunPod pod fleet

Read `.opencode/wiki/concepts/ollama-router-product.md`,
`.opencode/wiki/concepts/ollama-router-node-tunnel.md`, and
`.opencode/wiki/concepts/ollama-router-idle-scale-down.md`. Code:
`crates/ollama-router-runpod/`.

This skill covers **RunPod only**. Verda is a separate provider — see
`verda-spot-fleet`. Do not add Thunder. No RunPod Serverless / Instant
Clusters — **Pods only**.

## Auth

Bearer API key from env named by `runpod.api_key_env` (default `RUNPOD_API_KEY`).

- Never put the key in YAML.
- Never log `RUNPOD_API_KEY` or container env secret values.
- Honor 429 with `Retry-After` / `RateLimit` backoff.

When `runpod.enabled`, config load requires guest-reachable `runpod.enroll_url`
and `tunnel.api_endpoint` (absolute `http(s)`, not loopback/RFC1918/link-local).

## Selector (pure, unit-tested)

`GET /v2/catalog/gpus?include=AVAILABILITY` (v2 catalog). Filter available +
VRAM band + allow/deny GPU types + data centers + optional `max_price_per_hour`
(on-demand pre-filter). Rank ascending by price per known VRAM GiB; emit ordered
`gpuTypeIds`. Unknown VRAM loses a value comparison. DTOs: serde **ignore
extra fields**.

When `allowed_data_centers` is non-empty it is a **hard** filter. Otherwise
`preferred_data_centers` (default EU ids) is tried first; stockout falls back to
eligible offers outside that list.

## Provision URL

1. Create pod via v1 `POST /pods` with `interruptible` from config (default
   true). On-demand fallback only when `on_demand_fallback` is explicitly
   enabled — never silent.
2. Default image is **`ollama/ollama:latest`** (optional `template_id` →
   `templateId`). Bootstrap sets **`dockerEntrypoint: ["/bin/bash"]`** and
   **`dockerStartCmd: ["-lc", <script>]`** so the official Ollama entrypoint
   does not swallow the installer. The script starts
   `OLLAMA_HOST=127.0.0.1 ollama serve`, installs the agent package, enables a
   zrok **private** share, runs agent serve, and enrolls. Secrets arrive only
   via container `env` under configured `*_env` names — never in the script
   string, never logged. Agent package 404 → reason code + `sleep infinity`
   (no crash-loop).
3. Create with `ports: []`, `volumeInGb: 0`, tunable `containerDiskInGb`.
   Nothing is exposed on the RunPod proxy/public IP.
4. After create, verify actual `costPerHr` against `max_price_per_hour`; over
   cap → terminate immediately (stockout). Persist actual cost in FleetState.
5. After `RUNNING`, wait/reconcile polls FleetState enroll for the managed
   row, then `GET {tunnel}/api/tags`. Timeout uses allowlisted `enroll_timeout`,
   **terminates** the pod, and **keeps** FleetState ownership until destroy
   succeeds. Do not persist provider error text in SQLite.
6. Register the Ollama URL only after enroll of the zrok **private** share
   and `/api/tags` through the tunnel. Inside the pod, Ollama and the
   node-agent bind **loopback**.
7. Public pod IP / RunPod proxy hostnames are `public_url_blocked` — never a
   routing URL.

Logs on create: GPU type, data center, VRAM, price only — never script bodies,
enable tokens, admin bearers, or `RUNPOD_API_KEY`.

Ownership: pod name embeds a managed marker + router id; manage only pods
matching the scheme **and** a `managed_by=runpod` FleetState row. Foreign pods
are never touched. Enroll `origin=runpod` only updates an existing owned row
and must not write `fleet.yaml`.

## Destroy

Always **terminate** (`DELETE /pods/{id}`). Never stop-only (stopped pods
keep billing volume). Teardown failure retains FleetState ownership.
Interrupted / non-RUNNING managed pods are terminated each reconcile tick;
replace only when below `auto_scale_min_instances`.

## Demand

Implements `CloudProviderHandle` for `MultiProviderDemand` (cached best
offer refreshed on reconcile). Coalesced `create_additional` only — never
block the client; capacity miss stays **503 + Retry-After**.

## Tests

httpmock only. **No live RunPod.** Selector unit tests + proptest that the
cap/band filter dominates ranking. Manager suite: coalesced create, ceiling
refusal, foreign pod untouched, interruption below/above floor, failed destroy
retains row, enroll timeout terminates pod, no-secret logging on create failure.
