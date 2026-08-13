# ollama-router

CPU-only **Ollama-compatible fleet proxy** (Axum / Tokio). One listen URL
(`:11434`) load-balances generate, chat, and embed across env-configured hosts
and optional Verda Cloud NVIDIA **spot** GPUs.

This repo is **not** the Illumination Laravel app. Do not add Sail, PHP, Python
services, Thunder, or RunPod.

The Python tree at `/home/menes/Projects/illumination/services/ollama-router/`
is a behavioral spec — read it; do not paste it.

## Requirements

- **MSRV / pin:** rustc **1.97** (`rust-toolchain.toml`, `package.rust-version`)
- Edition 2021, committed `Cargo.lock`
- rustls only (`deny.toml` bans `openssl` / `native-tls`)

## Develop

Install [Task](https://taskfile.dev/). Local recipes live in `Taskfile.yml` (not Make).

```bash
task check          # fmt --check, clippy -D warnings, test --locked, cargo deny
cargo test --workspace --locked
task docker         # docker build -t ollama-router:local .
```

```bash
docker run --rm -p 11434:11434 ollama-router:local
# GET /healthz → 200 {"status":"ok","version":"0.1.0"}
```

CLI: `serve`, `ensure`, `delete`, `nodes`, `provision`. Only `serve` is
implemented in this skeleton; the rest parse and exit 2.

## Listen vs Sail

The binary/container listens on **`:11434`**. When Illumination Compose publishes
the optional `ollama-router` profile, the host mapping is
`${FORWARD_OLLAMA_ROUTER_PORT:-11435}:11434` so it does not collide with
host-installed Ollama. App traffic inside Sail stays `http://ollama-router:11434`.

This repo does not edit Illumination `compose.yaml`. When that stack switches
to this image, replace the Python `HEALTHCHECK` with curl/wget to `/healthz`.

## Follow-up

Shared `CapacityReport` / `Pressure` types with Illumination's
`ollama-capacity-agent` (`:11436`, GiB = `1024³`) stay a later extraction.
Do not vendor that tree here.
