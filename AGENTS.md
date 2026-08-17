# ollama-router

Mixed **CPU+GPU Ollama-compatible fleet proxy** (Axum / Tokio). The router
process needs **no GPU**. One listen URL (`:11434`) load-balances generate,
chat, and embed across `fleet.yaml` hosts and optional Verda Cloud NVIDIA
**spot** GPUs and/or RunPod interruptible GPU pods.

This repo is **not** the Illumination Laravel app. Do not add Sail, PHP, Python
services, or Thunder.

## Invariants

- **Cloud providers: Verda and RunPod.** Forbidden: Thunder, `THUNDER_*` env,
  `**/thunder/**`, Thunder admin routes or tests. RunPod is optional
  (`runpod:` tunables, `crates/ollama-router-runpod`, `RUNPOD_API_KEY`).
- **Inventory is fleet.yaml.** `OLLAMA_ROUTER_FLEET` (default
  `/etc/ollama-router/fleet.yaml`) + durable FleetState + Verda/RunPod managers.
  YAML tunables overlays are tunables-only. Top-level YAML `nodes:` is a hard
  config error (wrong file). Cloud spots/pods are not listed in fleet.yaml.
- **Tunnel/loopback-only cloud URLs.** Self-hosted zrok **private** share.
  Public `:11434` is `public_url_blocked` and never healthy. Hostname public
  tunnels (`*.zrok.io` etc.) are also rejected. There is no public-proxy
  exception. `fleet.yaml` LAN URLs stay direct HTTP. Enroll must not write
  `fleet.yaml`.
- **Idle timer = client forwards only.** `last_client_request_at` is written solely
  from `inflight_inc` on native generate / chat / embed **and** OpenAI
  `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`. Health, `/api/ps`, capacity
  probes, admin, and the warm-keeper do not count. After idle destroy: coalesced
  async cloud `create_additional` (best-value provider); the client gets **503 +
  `Retry-After`**. **Never destroy fleet.yaml hosts.** Destroy Verda instances
  with `delete_permanently`; terminate RunPod pods (never stop-only).
- **One router replica.** Two processes sharing one FleetState file can double-create
  cloud GPUs. The file lock is same-host only. Do not add Redis or run HA replicas.
- **Node agent on every Ollama host.** `crates/ollama-node-agent` (`setup` elevated,
  `serve` unprivileged on `:11436`). Probe `GET /v1/capacity` and `/v1/pressure`
  (plus `/v1/status`, `/metrics`). Soft-fail. GiB = bytes / `1024³`. Shared DTOs
  in `crates/ollama-capacity-types`. The router does not install or supervise
  Ollama.
- **Sensitivity.** Never log bodies, prompts, embeddings, zrok share tokens,
  Verda tokens/secrets, `RUNPOD_API_KEY`, SSH keys, or the admin bearer.

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
| `crates/ollama-router-runpod/src/` | Bearer REST client, selector, manager (terminate permanently) |
| `crates/ollama-router-core/src/config/` | YAML tunables + env knobs; reject `nodes:` |
| `crates/ollama-router-core/src/fleet/` | Registry, fleet.yaml inventory, FleetState |
| `crates/ollama-mock/` | Compose CPU/GPU Ollama mock (no inference) |
| `crates/ollama-router-core/src/cloud/` | Idle reconcile + multi-provider demand |
| `crates/ollama-router-core/src/capacity/` | HTTP client to `:11436` |
| `crates/ollama-capacity-types/` | Shared `CapacityReport` / pressure JSON |
| `crates/ollama-node-agent/` | Node setup + `serve` on `:11436`; setup installs Ollama and spawns zrok sidecar |
| `crates/ollama-router-core/src/jobs/` | SQLite durable pull/delete |

Workspace root: committed `Cargo.lock`, edition **2021**, rustc pin **1.97**.

## Stack lock

- Axum **0.8** + Tokio + tower-http + reqwest `rustls`
- **No** `native-tls`, **no** `openssl` / `openssl-sys`
- tracing JSON (not `println!`, not Python structlog)
- Verda/RunPod DTOs: serde **ignore unknown fields** (`deny_unknown_fields` is wrong)
- `thiserror` in libraries; `anyhow` only in the binary
- No `unwrap` / `expect` in non-test lib code
- Admin bearer fail-closed: unset token → admin API disabled (403), no default secret

## Commands

Local runner is **Task** ([taskfile.dev](https://taskfile.dev/)) — `Taskfile.yml`. Never add a Makefile or justfile.

```bash
task check          # fmt --check, clippy -D warnings, test --locked, cargo deny
task coverage       # cargo llvm-cov --fail-under-lines 80 (≥80% lines; ignore **/main.rs)
task docker         # docker build -t ollama-router:local . (or: docker buildx bake router)
task dev            # host Ollama :11434 → router :11435 + agent :11436
task compose:up     # Grafana :3000 / Prometheus :9090 (scrapes host :11435)
task compose:mock   # optional canned CPU+GPU mock fleet on host :11435
task agent:doctor   # read-only node-agent report for this machine
task agent:release  # host-OS agent packages into dist/agent (Linux: Docker rust:1.97.1-slim-bookworm; GHA never installs task)
```

The agent serve image is a target of the root `Dockerfile` (`docker build
--target agent`), not a separate `Dockerfile.agent`. Bake targets `router` /
`mock` / `agent` live in `docker-bake.hcl`; CI and GHCR publish `router` only.
Observability (Grafana/Alloy/Loki/Alertmanager + `grafana-data`) is shared
between `deploy/compose.yaml` and `deploy/compose.mock.yaml` via
`deploy/observability/compose.stack.yaml`.

Before finishing a coding task, run `task check` and (for Rust/test changes) `task coverage`; do not stop while either fails. Line coverage must stay **≥ 80%**.

Remote hosts: install `ollama-node-agent` and run `setup` (Ollama + zrok sidecar
spawned from the zrok binary). `setup`/`doctor` print a find-this-node block
(share token **id** + enroll status, never the raw share); the router learns
the share via enroll (FleetState only — never `fleet.yaml`, never SSH). Verda
bootstrap is a startup script; RunPod bootstrap is container `dockerStartCmd`
(no SSH). The router may still upload an SSH public key to satisfy Verda's API
but must never SSH. See `.opencode/wiki/concepts/ollama-router-node-tunnel.md`.

CI runs the same cargo commands directly (no `task` binary on GitHub):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check advisories bans
cargo llvm-cov --workspace --locked --fail-under-lines 80 --summary-only --ignore-filename-regex '(^|/)main\.rs$'
```

Dockerfile: multi-stage `rust:1.97.1-slim-bookworm` → `debian:bookworm-slim`, non-root `router` (uid 1000), **HEALTHCHECK** `curl` to `/healthz` (never Python). Listen `:11434` in-container. `router` / `mock` / `agent` are targets of this one Dockerfile. `/router/ui` is served from rust-embed of `crates/ollama-router/ui/dist` (Rust never lists Vite asset filenames).

## Learned User Preferences

- Keep Context7 global; project `.cursor/mcp.json` / `.opencode/opencode.json` may pin docsrs, grafana, and prometheus for local compose (`GRAFANA_URL`/`PROMETHEUS_URL` only — never commit API keys or tokens). OpenCodeRAG is OpenCode-only — do not register it in Cursor MCP or call its tools from Cursor.
- Pin GitHub Actions to version tags (`actions/checkout@v7`, `Swatinem/rust-cache@v2`, `github/codeql-action@v4`), never commit SHAs.
- Prefer Grafana Alloy over a pile of promtails for local compose log shipping; do not add Elasticsearch, Jaeger, or Zipkin unless OTLP/Tempo is explicitly required.
- Keep the fleet overview Grafana dashboard (`ollama-router.json`) as the compose home dashboard; additive dashboards (nodes, jobs, etc.) must not replace it or change `GF_DASHBOARDS_DEFAULT_HOME_DASHBOARD_PATH`.
- Treat `fleet.yaml` as GitOps source of truth for permanent hosts; admin `PUT /router/v1/nodes` and enroll are debug/adopt only and must not write `fleet.yaml`.
- Node-agent is headless only: no Tauri, tray, `.app`, or webview; OS packages wrap the same `setup`/`serve` binary.
- Cloud Ollama URLs are a self-hosted zrok **private** share (not Tailscale, not `zrok.io`, not public shares). Verda bootstrap is a startup script; RunPod bootstrap is `dockerStartCmd`; the router never SSH. Enroll does not write `fleet.yaml`. `nodes` CLI prints `origin`, `id`, `url`, `tunnel_backend`, `enroll_age` — never share tokens. Optional Prometheus gauge `ollama_router_tunnel_up{node}`; scrape the router only (never node-agent `:11436`).

## Learned Workspace Facts

- cargo-deny `[bans].allow` is an exclusive allowlist; omit it and only `[bans].deny` openssl, openssl-sys, and native-tls.
- On this private repo without GitHub code scanning enabled, gate artifact attestations with `if: ${{ !github.event.repository.private }}` and gate CodeQL SARIF/`upload-database` (e.g. `CODEQL_SHOULD_UPLOAD`) so analysis can still run as workflow artifacts.
- Trust the node-agent `pressure_level`; do not port Python `classify_pressure` or RAM classify knobs into the router. Classification lives in `ollama-node-agent`. Keep VRAM/RAM headroom and the reservation ledger.
- Agent JSON must ignore unknown fields (the agent may add columns). `deny_unknown_fields` is for our YAML only (tunables + fleet.yaml + agent config), not Verda/RunPod or capacity payloads.
- Verda `ssh_public_key_file` / `ssh_private_key_file` exist only to satisfy a possible Verda `ssh_key_ids` API constraint — never add an SSH key env var, and the router must never SSH.
- Verda `ensure` is adopt-first; demand scale coalesces `create_additional` only (never `ensure`). Tag instances `managed_by=ollama-router` and reject `illumination-*`; FleetState ownership is `managed_by=verda` or `managed_by=runpod`.
- Hot Prometheus metrics use the `prometheus` crate in the binary only and must not label by model name; `/metrics` and `/healthz` stay unauthenticated. Scrape the router only — do not add a Prometheus job for node-agent `:11436`. Treat `vram_free_gb=0` as unknown unless `vram_free_known`; do not add labels to `node_info` (`node`, `origin`, `role`).
- Model jobs: `auto_pull_on_miss` exists (default **false**, placement-gated via `static_capacity_fits`); still no `unsafe_single_node_mutate`. SQLite stores operation metadata only (no bodies or provider error text).
- Local-dev is native `task dev` (host Ollama `:11434`, router `:11435`, agent `:11436`). `task compose:up` is Grafana `:3000` / Prometheus `:9090` only. `task compose:mock` is the canned fleet.
- Node-agent GPU discovery: NVIDIA (`nvidia-smi`) and AMD ROCm (`rocm-smi`/`amd-smi`) are first-class; Auto order is NVIDIA inventory → macOS Metal → ROCm → CPU; never encode unmeasured VRAM as `0`.
- Node-agent packaging: portable Linux tar.gz + `.deb` (nfpm), Windows MSI+SCM (not schtasks), macOS `.pkg`+LaunchDaemon; Linux `setup` must succeed without systemd (manual `serve`); ship via `task agent:release` / `release-agent.yml` (no Tauri).

<!-- BEGIN opencode-rag -->
## OpenCodeRAG (OpenCode only)

This workspace indexes via `opencode-rag.json` for the **OpenCode plugin**. Those
tools are not in Cursor.

- **Cursor:** do not call `search_semantic`, `get_file_skeleton`, `find_usages`,
  `describe_image`, or quirk tools (`add_quirk` / `recall_quirks` / `update_quirk`
  / `delete_quirk`). Use Grep, Read, Glob, and IDE search.
  Do not run `opencode-rag mcp` or put RAG in `.cursor/mcp.json`.
- **OpenCode:** the plugin injects those tools at runtime. Follow
  `.opencode/skills/opencode-rag/SKILL.md`.

`opencode-rag init` may restore a longer “ALWAYS use OpenCodeRAG tools” block
here. Cursor’s `.cursor/rules/opencode-rag.mdc` is the durable override.
<!-- END opencode-rag -->
