---
title: Installation
description: Install the router and the node agent — Docker, binary, or OS packages.
sidebar:
  order: 2
---

## Router

The router process needs no GPU. It ships as a container and as a binary.

**Docker** (recommended):

```bash
task docker
docker run --rm -p 11434:11434 ollama-router:local
```

The image is multi-stage `rust:1.97.1-slim-bookworm` → `debian:bookworm-slim`,
runs as non-root `router` (uid 1000), listens on `:11434` in-container, and has
a curl-based `HEALTHCHECK` against `/healthz`.

**Binary**: build with `cargo build -p ollama-router --locked --release` and
run `ollama-router serve`. The listen address comes from `OLLAMA_ROUTER_HOST`
/ `OLLAMA_ROUTER_PORT` (default `0.0.0.0` / `11434`).

Key environment knobs:

| Knob | Default |
| --- | --- |
| `OLLAMA_ROUTER_FLEET` | `/etc/ollama-router/fleet.yaml` |
| `OLLAMA_ROUTER_CONFIG` | unset — optional tunables overlay |
| `OLLAMA_ROUTER_ADMIN_TOKEN` | unset → admin API **403** (no default secret) |

## Node agent

Every Ollama host runs `ollama-node-agent`:

- `setup` — privileged, idempotent; installs/verifies Ollama and converges
  the host config (systemd / LaunchDaemon / Windows Service).
- `serve` — unprivileged, serves capacity JSON on `:11436`
  (`/v1/capacity`, `/v1/pressure`, `/v1/status`, `/metrics`).

The router does **not** install or supervise Ollama. Do not bind `:11436` to
`0.0.0.0` without a bearer token.

OS packages (see the release workflow; `task agent:release` locally):

| OS | Install |
| --- | --- |
| Linux amd64/arm64 | `ollama-node-agent_<ver>_<arch>.deb` or musl static tarball |
| macOS amd64/arm64 | `.pkg` (LaunchDaemon `com.ollama.node-agent`) |
| Windows amd64 | `.msi` (LocalSystem SCM) |

Packages install the **agent** only — run `sudo ollama-node-agent setup` to
converge Ollama. Without systemd, `setup` still succeeds and prints how to run
`serve` manually.

## zrok private share (cloud only)

Cloud hosts reach the router over a **self-hosted zrok private share**
(`ZROK_API_ENDPOINT` + `ZROK_ENABLE_TOKEN` in the environment, never in YAML).
See the [cloud guide](/guides/cloud/) and `deploy/zrok/README.md`. This is not
a second VPN and not a public tunnel.
