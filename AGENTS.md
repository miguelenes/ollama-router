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
  probes, admin, and the warm-keeper do not count. After idle destroy: async Verda
  `ensure`; the client gets **503 + `Retry-After`**. **Never destroy fleet.yaml
  hosts.** Destroy Verda instances with `delete_permanently`.
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
```

Before finishing a coding task, run `task check` and do not stop while it fails.

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

## Learned Workspace Facts

- cargo-deny `[bans].allow` is an exclusive allowlist; omit it and only `[bans].deny` openssl, openssl-sys, and native-tls.
- GitHub artifact attestations fail on user-owned private repos; gate `actions/attest-build-provenance` with `if: ${{ !github.event.repository.private }}` until the repo is public.
- Trust the capacity-agent `pressure_level`; do not port Python `classify_pressure` or RAM classify knobs (`ram_elevated_*`, `ram_swap_*`, `ram_load_*`). Keep VRAM/RAM headroom and the reservation ledger.
- Capacity-agent JSON must ignore unknown fields (the agent may add columns). `deny_unknown_fields` is for our YAML only (tunables + fleet.yaml), not Verda or agent payloads.

<!-- BEGIN opencode-rag -->
## Code Navigation

ALWAYS use OpenCodeRAG tools before reading or editing:
- **Search first** — `search_semantic(query)` instead of grep/glob
- **Skeleton before read** — `get_file_skeleton(filePath)` then read specific lines
- **Usages before edit** — `find_usages(symbolName)` before modifying any symbol
- **Images via describe** — `describe_image(filePath, systemPrompt?)` — never read raw bytes
- **Recall quirks** — `recall_quirks(query)` when you hit a known pitfall
- **Add quirks** — `add_quirk(content)` when you discover a non-obvious fact
- **Fix quirks** — `update_quirk(id, ...)` / `delete_quirk(id)` when a stored quirk is outdated or wrong

If no results, run `opencode-rag index`.

### Decision tree — ALWAYS follow this order
1. User mentions code behavior/architecture → `search_semantic(query)`
2. User mentions a file path → `get_file_skeleton(filePath)` THEN `read` on specific lines
3. User mentions a function/class/variable to edit → `find_usages(symbolName)` THEN `search_semantic` THEN `edit`
4. User asks a code question → `search_semantic` to gather context before answering
5. User asks about an image or visual asset → `describe_image(filePath)` (optionally pass `systemPrompt` to focus on specific features) to retrieve its generated description, then optionally `search_semantic` for related code
6. You encounter an error or need to recall a known pitfall → `recall_quirks(query)`
7. You discover a non-obvious fact or workaround → `add_quirk(content)` to persist it for future sessions
8. A recalled quirk is outdated or wrong → `update_quirk(id, ...)` to fix it, or `delete_quirk(id)` if it no longer applies

### Proactive triggers — you MUST call these tools when
- User asks about code behavior, architecture, or implementation details
- User asks to edit, refactor, or fix code — call `find_usages` first
- User references files or functions you haven't read yet
- User says "find", "search", "look up", "where is", "how does"
- User refers to an image, screenshot, diagram, or visual asset
- Before answering ANY code-related question, retrieve context first
- Before reading ANY file, call `get_file_skeleton` to orient first

### Anti-patterns — NEVER do these
- Reading full files without calling `get_file_skeleton` first (wastes tokens)
- Editing a function without calling `find_usages` first (breaks call sites)
- Answering code questions without calling `search_semantic` first (you guess at behavior)
- Using `grep`/`glob` when `search_semantic` would find the answer faster
- Treating image files as text — use `describe_image` instead of reading raw bytes
- Using `npx opencode-rag quirk` shell commands instead of the built-in quirk tools (`add_quirk` / `recall_quirks` / `update_quirk` / `delete_quirk`) (the tools are faster, already loaded in-process, and go through the trust monitor)

### MANDATORY quirk capture rules — you MUST call `add_quirk` when
- A build, test, or type-check command fails and you resolve it
- You discover an undocumented library constraint, peer dep, or workaround
- You learn an environment-specific requirement (OS, tool version, etc.)
- You make a design decision that future sessions should remember
- You resolve a gotcha that cost more than one attempt

### MANDATORY quirk hygiene — you MUST call `update_quirk` or `delete_quirk` when
- A stored quirk is outdated, wrong, or has been fixed — update it or delete it instead of adding a contradicting duplicate
- NEVER finish a coding session without adding quirks for resolved errors.
<!-- END opencode-rag -->
