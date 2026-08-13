<p align="center">
  <img src="docs/assets/mark.svg" width="72" height="72" alt="ollama-router mark">
</p>

<p align="center">
  <img src="docs/assets/banner.svg" width="880" alt="ollama-router — one URL, many Ollama hosts">
</p>

<p align="center">
  <strong>CPU-only Ollama-compatible fleet proxy.</strong><br>
  One listen URL. Env hosts plus optional Verda Tailscale GPUs.
</p>

<p align="center">
  <a href="https://github.com/miguelenes/ollama-router/actions/workflows/ci.yml"><img src="https://github.com/miguelenes/ollama-router/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/status-0.1.0%20preview-f59e0b?style=flat-square" alt="0.1.0 preview">
  <img src="https://img.shields.io/badge/rustc-1.97-dea584?style=flat-square&logo=rust&logoColor=white" alt="rustc 1.97">
  <img src="https://img.shields.io/badge/edition-2021-1c2733?style=flat-square" alt="edition 2021">
  <img src="https://img.shields.io/badge/axum-0.8-2dd4bf?style=flat-square" alt="Axum 0.8">
  <img src="https://img.shields.io/badge/tls-rustls-0f766e?style=flat-square" alt="rustls only">
  <img src="https://img.shields.io/badge/docker-HEALTHCHECK-2496ed?style=flat-square&logo=docker&logoColor=white" alt="Docker HEALTHCHECK">
  <img src="https://img.shields.io/badge/license-proprietary-6b7280?style=flat-square" alt="proprietary license">
</p>

<p align="center">
  <a href="#quick-start">Quick start</a>
  &nbsp;·&nbsp;
  <a href="#architecture">Architecture</a>
  &nbsp;·&nbsp;
  <a href="#inventory">Inventory</a>
  &nbsp;·&nbsp;
  <a href="#develop">Develop</a>
</p>

> [!NOTE]
> **0.1.0 preview.** `serve` and `/healthz` ship. The sections below are the product contract this rewrite is landing.

## What it is

Clients speak ordinary Ollama to **one** listen URL (`:11434`). The router load-balances generate, chat, and embed across a fleet you already run, then optionally bursts onto Verda NVIDIA **spot** GPUs on Tailscale.

- **Ollama surface** — `POST /api/generate`, `/api/chat`, `/api/embed` (plus `/api/embeddings` rewritten to `/api/embed` for Ollama ≤0.32). Aggregated `GET /api/tags`. NDJSON streaming.
- **Utilization WLC** — rank by `inflight / capacity`, then RAM pressure, then class preference (embed / small / medium / large). Saturated nodes are never a fallback.
- **Env-first inventory** — `OLLAMA_HOST_NN_*` + durable FleetState + Verda. YAML is tunables only.
- **Verda spots** — cheapest, then smallest GPU inside an inclusive 8–80 GiB VRAM window. Public `:11434` is never healthy.
- **Idle teardown** — router-owned. Only proxied client forwards reset the timer. Health, `/api/ps`, capacity, admin, and the warm-keeper do not count.
- **Capacity miss** — coalesced async Verda `ensure`. The client gets **503 + `Retry-After`**, never a blocked provision.

The sibling capacity agent on `:11436` is not this crate. GiB = bytes / `1024³`.

## Architecture

```mermaid
flowchart LR
  clients[Clients] --> router["ollama-router :11434"]
  router --> envHosts[OLLAMA_HOST_NN]
  router --> verda[Verda spots]
  envHosts --> agent["capacity-agent :11436"]
  verda --> agent
```

Cloud Ollama URLs are Tailscale-only. Register a node after OpenSSH and `/api/tags` succeed on the tailnet. Capacity probes are soft-fail and never fake client activity.

## Inventory

Fleet membership is **not** a YAML list.

| Source | Role |
| --- | --- |
| `OLLAMA_HOST_NN_*` | Primary static hosts (`01`–`99`) |
| FleetState | Durable Tailscale URLs and Verda metadata |
| Verda manager | Dynamic spot GPUs |
| `OLLAMA_ROUTER_NODES` | Compact test/dev override only |

Point `OLLAMA_ROUTER_CONFIG` at a thin overlay such as [`config.overlay.example.yaml`](config.overlay.example.yaml) to override committed tunables in `router.defaults.yaml`. A top-level `nodes:` key is a **hard config error**.

| Knob | Default |
| --- | --- |
| `OLLAMA_ROUTER_HOST` / `OLLAMA_ROUTER_PORT` | `0.0.0.0` / `11434` |
| `OLLAMA_ROUTER_ADMIN_TOKEN` | unset → admin API **403** (no default secret) |

Verda stays **off** until you enable it. Credentials belong in process env, never in YAML.

## Quick start

Install [Task](https://taskfile.dev/), then:

```bash
task docker
docker run --rm -p 11434:11434 ollama-router:local
curl -fsS http://127.0.0.1:11434/healthz
# {"status":"ok","version":"0.1.0"}
```

Equivalent: `docker build -t ollama-router:local .`

When this image is published next to a Compose stack, map the host as `${FORWARD_OLLAMA_ROUTER_PORT:-11435}:11434` so it does not collide with host-installed Ollama. In-network clients keep `http://ollama-router:11434`. This repo does not edit that Compose file.

## Develop

Local recipes live in [`Taskfile.yml`](Taskfile.yml) (not Make). `check` is sequential so cargo steps do not contend on `target/`.

```bash
task check          # fmt --check, clippy -D warnings, test --locked, cargo deny
cargo test --workspace --locked
```

- **MSRV:** rustc **1.97** (`rust-toolchain.toml`)
- Edition 2021, committed `Cargo.lock`
- rustls only (`deny.toml` bans `openssl` / `native-tls`)
- tracing JSON — never request bodies, prompts, embeddings, or tokens

CLI: `serve`, `ensure`, `delete`, `nodes`, `provision`. **`serve` is implemented.** The rest parse and exit 2 until the fleet proxy lands.

Workspace: `crates/ollama-router` (binary / HTTP), `crates/ollama-router-core` (config, fleet, routing, capacity, jobs), `crates/ollama-router-verda` (OAuth2 + spot manager).

## Sensitivity

Never log or persist prompts, embeddings, `/api/chat` messages, Verda tokens, Tailscale auth keys, SSH private keys, or `OLLAMA_ROUTER_ADMIN_TOKEN`.

Allowlisted operational fields: node id, model name, request class, status, latency, reason codes (`no_healthy`, `saturated`, `public_url_blocked`), instance type, location, spot price, VRAM GiB.

SQLite (`/var/lib/ollama-router/model-operations.sqlite3`) keeps operation id, kind, status, timestamps, and normalized models/nodes. Upstream bodies stay memory-only.

## License

Copyright © ollama-router authors. All rights reserved. This software is **proprietary**; see `license = "proprietary"` in [`Cargo.toml`](Cargo.toml).
