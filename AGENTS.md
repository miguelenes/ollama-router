# ollama-router

Mixed **CPU+GPU Ollama-compatible fleet proxy** (Axum / Tokio). The router
process needs **no GPU**. One listen URL (`:11434`) load-balances generate,
chat, and embed across `fleet.yaml` hosts and optional Verda Cloud NVIDIA
**spot** GPUs.

This repo is **not** the Illumination Laravel app. Do not add Sail, PHP, Python
services, Thunder, or RunPod.

## Invariants

- **Verda spots only.** Forbidden: Thunder, RunPod, `THUNDER_*` / `RUNPOD_*` env,
  `**/thunder/**`, `**/runpod/**`, admin routes or tests for those providers.
- **Inventory is fleet.yaml.** `OLLAMA_ROUTER_FLEET` (default
  `/etc/ollama-router/fleet.yaml`) + durable FleetState + Verda manager.
  YAML tunables overlays are tunables-only. Top-level YAML `nodes:` is a hard
  config error (wrong file). Verda spots are not listed in fleet.yaml.
- **Tailscale-only cloud URLs.** Public `:11434` is `public_url_blocked` and never
  healthy. There is no public-proxy exception.
- **Idle timer = client forwards only.** `last_client_request_at` is written solely
  from `inflight_inc` on generate / chat / embed. Health, `/api/ps`, capacity
  probes, admin, and the warm-keeper do not count. After idle destroy: coalesced
  async Verda `create_additional`; the client gets **503 + `Retry-After`**. **Never
  destroy fleet.yaml hosts.** Destroy Verda instances with `delete_permanently`.
- **One router replica.** Two processes sharing one FleetState file can double-create
  Verda spots. The file lock is same-host only. Do not add Redis or run HA replicas.
- **Node agent on every Ollama host.** `crates/ollama-node-agent` (`setup` elevated,
  `serve` unprivileged on `:11436`). Probe `GET /v1/capacity` and `/v1/pressure`
  (plus `/v1/status`, `/metrics`). Soft-fail. GiB = bytes / `1024³`. Shared DTOs
  in `crates/ollama-capacity-types`. The router does not install or supervise
  Ollama.
- **Sensitivity.** Never log bodies, prompts, embeddings, Tailscale keys, Verda
  tokens/secrets, SSH keys, or the admin bearer.

## Behavioral spec (read, do not paste)

Python tree (behavior only):
`/home/menes/Projects/illumination/services/ollama-router/`

Node agent (this repo): `crates/ollama-node-agent` (Axum 0.8, edition 2021,
tracing JSON). Historical Illumination `ollama-capacity-agent` is behavior
reference only — do not port it blindly.

Deep design: `.opencode/wiki/concepts/` (this repo). Product and imported
skills: `.cursor/skills/` and `.opencode/skills/` (canonical CLI store:
`.agents/skills/` + `skills-lock.json`).

Fetch Axum 0.8 / Tokio / reqwest / tower-http docs via **Context7** before coding
those APIs. Use the **docsrs-mcp** skill (`lookup_item` / `search_crate`) for
rustdoc signatures and trait impls. There is no Axum 0.8 skill; do not follow
Axum 0.7 GraphQL/WS guides.

## Crate layout (implement later; names are load-bearing)

| Path | Role |
|------|------|
| `crates/ollama-router/src/http/` | Axum app: `/healthz`, `/readyz`, `/metrics`, admin `/router/v1/*` |
| `crates/ollama-router/src/proxy/` | NDJSON streaming; `/api/embeddings` → `/api/embed` |
| `crates/ollama-router-core/src/routing/` | Utilization WLC + class preference (pure fns) |
| `crates/ollama-router-verda/src/` | OAuth2 client, selector, manager (`delete_permanently`) |
| `crates/ollama-router-core/src/config/` | YAML tunables + env knobs; reject `nodes:` |
| `crates/ollama-router-core/src/fleet/` | Registry, fleet.yaml inventory, FleetState |
| `crates/ollama-mock/` | Compose CPU/GPU Ollama mock (no inference) |
| `crates/ollama-router-core/src/cloud/` | Idle reconcile (Verda-only manager) |
| `crates/ollama-router-core/src/capacity/` | HTTP client to `:11436` |
| `crates/ollama-capacity-types/` | Shared `CapacityReport` / pressure JSON |
| `crates/ollama-node-agent/` | Node setup + `serve` on `:11436` |
| `crates/ollama-router-core/src/jobs/` | SQLite durable pull/delete |
| `crates/ollama-router/src/provision/` | russh + Tailscale handoff (no OpenSSH binary) |

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

Local runner is **Task** ([taskfile.dev](https://taskfile.dev/)) — `Taskfile.yml`. Never add a Makefile or justfile.

```bash
task check          # fmt --check, clippy -D warnings, test --locked, cargo deny
task docker         # docker build -t ollama-router:local .
task compose:up     # mock CPU+GPU fleet on host :11435
task agent:doctor   # read-only node-agent report for this machine
```

Before finishing a coding task, run `task check` and do not stop while it fails.

SSH provision of fleet hosts should eventually upload `ollama-node-agent` and run
`setup` rather than embedding Ubuntu Ollama logic in bash. The russh provisioner
still uses `provision-ollama-gpu.sh` until that handoff lands.

CI runs the same cargo commands directly (no `task` binary on GitHub):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check advisories bans
```

Dockerfile: multi-stage `rust:1.97-slim-bookworm` → `debian:bookworm-slim`, non-root `router` (uid 1000), **HEALTHCHECK** `curl` to `/healthz` (never Python). Listen `:11434` in-container.

## Learned User Preferences

- Keep Context7 and docsrs-mcp as global Cursor/OpenCode MCP servers; leave project `.cursor/mcp.json` `mcpServers` empty and never commit API keys there.
- Pin GitHub Actions to version tags (`actions/checkout@v7`, `Swatinem/rust-cache@v2`, `github/codeql-action@v4`), never commit SHAs.
- Prefer Grafana Alloy over a pile of promtails for local compose log shipping; do not add Elasticsearch, Jaeger, or Zipkin unless OTLP/Tempo is explicitly required.
- Treat `fleet.yaml` as GitOps source of truth for permanent hosts; admin `PUT /router/v1/nodes` is debug/adopt only and must not write `fleet.yaml`.

## Learned Workspace Facts

- cargo-deny `[bans].allow` is an exclusive allowlist; omit it and only `[bans].deny` openssl, openssl-sys, and native-tls.
- russh must be **>=0.60.3** (we pin **0.62**): 0.54.5 fails deny on RUSTSEC-2026-0154/0153. `RUSTSEC-2023-0071` (rsa Marvin) has no patch — ignored in `deny.toml` because rsa is only pulled via russh.
- On this private repo without GitHub code scanning enabled, gate artifact attestations with `if: ${{ !github.event.repository.private }}` and gate CodeQL SARIF/`upload-database` (e.g. `CODEQL_SHOULD_UPLOAD`) so analysis can still run as workflow artifacts.
- Trust the node-agent `pressure_level`; do not port Python `classify_pressure` or RAM classify knobs into the router. Classification lives in `ollama-node-agent`. Keep VRAM/RAM headroom and the reservation ledger.
- Agent JSON must ignore unknown fields (the agent may add columns). `deny_unknown_fields` is for our YAML only (tunables + fleet.yaml + agent config), not Verda or capacity payloads.
- SSH private keys come from `ssh.key_file` / Compose secrets / Verda `ssh_private_key_file` only — never add an SSH key env var.
- Verda `ensure` is adopt-first; demand scale coalesces `create_additional` only (never `ensure`). Tag instances `managed_by=ollama-router` and reject `illumination-*`; FleetState ownership is `managed_by=verda`.
- Hot Prometheus metrics use the `prometheus` crate in the binary only and must not label by model name; `/metrics` and `/healthz` stay unauthenticated.
- Model jobs: `auto_pull_on_miss` exists (default **false**, placement-gated via `static_capacity_fits`); still no `unsafe_single_node_mutate`. SQLite stores operation metadata only (no bodies or provider error text).
- Local compose publishes the router on host `:11435`; document Grafana `:3000` and Prometheus `:9090`.

<!-- BEGIN opencode-rag -->
## OpenCodeRAG (OpenCode only)

This workspace indexes via `opencode-rag.json` for the **OpenCode plugin**. Those
tools are not in Cursor.

- **Cursor:** do not call `search_semantic`, `get_file_skeleton`, `find_usages`,
  `describe_image`, or quirk tools (`add_quirk` / `recall_quirks` / `update_quirk`
  / `delete_quirk`). Use Grep, Glob, Read, and IDE search. Memory is MemoryAI.
  Do not run `opencode-rag mcp` or put RAG in `.cursor/mcp.json`.
- **OpenCode:** the plugin injects those tools at runtime. Follow
  `.opencode/skills/opencode-rag/SKILL.md`.

`opencode-rag init` may restore a longer “ALWAYS use OpenCodeRAG tools” block
here. Cursor’s `.cursor/rules/opencode-rag.mdc` is the durable override.
<!-- END opencode-rag -->
