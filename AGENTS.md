# ollama-router

CPU-only **Ollama-compatible fleet proxy** (Axum / Tokio). One listen URL (`:11434`)
load-balances generate, chat, and embed across env-configured hosts and optional
Verda Cloud NVIDIA **spot** GPUs.

This repo is **not** the Illumination Laravel app. Do not add Sail, PHP, Python
services, Thunder, or RunPod.

## Invariants

- **Verda spots only.** Forbidden: Thunder, RunPod, `THUNDER_*` / `RUNPOD_*` env,
  `**/thunder/**`, `**/runpod/**`, admin routes or tests for those providers.
- **Inventory is env-first.** `OLLAMA_HOST_NN_*` + durable FleetState + Verda
  manager. YAML is tunables-only. Top-level YAML `nodes:` is a hard config error.
  Compact test override: `OLLAMA_ROUTER_NODES` only.
- **Tailscale-only cloud URLs.** Public `:11434` is `public_url_blocked` and never
  healthy. There is no public-proxy exception.
- **Idle timer = client forwards only.** `last_client_request_at` is written solely
  from `inflight_inc` on generate / chat / embed. Health, `/api/ps`, capacity
  probes, admin, and the warm-keeper do not count. After idle destroy: async Verda
  `ensure`; the client gets **503 + `Retry-After`**. Env permanent hosts are never
  torn down. Destroy Verda instances with `delete_permanently`.
- **Capacity agent is a sibling, not this crate.** Probe
  `GET http://{ollama-host}:11436/v1/capacity` and `/v1/pressure`. Soft-fail.
  GiB = bytes / `1024³`. Do not reimplement the agent here.
- **Sensitivity.** Never log bodies, prompts, embeddings, Tailscale keys, Verda
  tokens/secrets, SSH keys, or the admin bearer.

## Behavioral spec (read, do not paste)

Python tree (behavior only):
`/home/menes/Projects/illumination/services/ollama-router/`

Capacity-agent wire contract (Axum 0.8, edition 2021, tracing JSON):
`/home/menes/Projects/illumination/services/ollama-capacity-agent/`

Deep design: `.opencode/wiki/concepts/` (this repo). Product and imported
skills: `.cursor/skills/` and `.opencode/skills/` (canonical CLI store:
`.agents/skills/` + `skills-lock.json`).

Fetch Axum 0.8 / Tokio / reqwest / tower-http docs via **Context7** before coding
those APIs. There is no Axum 0.8 skill; do not follow Axum 0.7 GraphQL/WS guides.

## Crate layout (implement later; names are load-bearing)

| Path | Role |
|------|------|
| `crates/ollama-router/src/http/` | Axum app: `/healthz`, `/readyz`, `/metrics`, admin `/router/v1/*` |
| `crates/ollama-router/src/proxy/` | NDJSON streaming; `/api/embeddings` → `/api/embed` |
| `crates/ollama-router-core/src/routing/` | Utilization WLC + class preference (pure fns) |
| `crates/ollama-router-verda/src/` | OAuth2 client, selector, manager (`delete_permanently`) |
| `crates/ollama-router-core/src/config/` | YAML tunables + env knobs; reject `nodes:` |
| `crates/ollama-router-core/src/fleet/` | Registry, env inventory, FleetState |
| `crates/ollama-router-core/src/cloud/` | Idle reconcile (Verda-only manager) |
| `crates/ollama-router-core/src/capacity/` | HTTP client to `:11436` |
| `crates/ollama-router-core/src/jobs/` | SQLite durable pull/delete |

Workspace root: committed `Cargo.lock`, edition **2021**, rustc pin **1.97**.

## Stack lock

- Axum **0.8** + Tokio + tower-http + reqwest `rustls-tls`
- **No** `native-tls`, **no** `openssl` / `openssl-sys`
- tracing JSON (not `println!`, not Python structlog)
- Verda DTOs: serde **ignore unknown fields** (`deny_unknown_fields` is wrong)
- `thiserror` in libraries; `anyhow` only in the binary
- No `unwrap` / `expect` in non-test lib code
- Admin bearer fail-closed: unset token → admin API disabled (403), no default secret

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check advisories bans
```

Later Dockerfile (not in this pass): multi-stage `rust:1.97-slim` → debian-slim,
non-root user, **HEALTHCHECK must not use Python**.
