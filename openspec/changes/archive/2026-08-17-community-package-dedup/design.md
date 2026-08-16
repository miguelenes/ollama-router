## Context

See `proposal.md` for motivation. Today: `serde_yaml` 0.9 parses tunables and fleet.yaml with `Value` deep-merge plus `deny_unknown_fields`; ~15 `reqwest::Client::builder().use_rustls_tls()` sites; Verda and RunPod each parse `Retry-After`; router and node-agent duplicate JSON tracing init; `/router/ui` hard-codes Vite `assets/app.js` and `assets/index.css`; `Dockerfile` and `Dockerfile.agent` both install cargo-chef 0.1.78 on `rust:1.97.1-slim-bookworm`; `compose.yaml` and `compose.mock.yaml` copy Grafana/Alloy/Loki/Alertmanager; GitHub workflows repeat checkout + `dtolnay/rust-toolchain@stable` (1.97) + `Swatinem/rust-cache@v2`.

Constraints that shape the approach: rustls only (`deny.toml` bans openssl/native-tls); inference is NDJSON/SSE and must not be buffered or generically retried; YAML overlays remain tunables-only (`nodes:` is a hard error); one router replica; no Redis/HA; scrape router `:11435` only; Taskfile stays the local runner (no Makefile, no `task` in GHA); ranking / unknown-VRAM vs measured CPU is out of scope.

Context7 (2026-08-16): reqwest 0.13 `Client` is `Clone` over an inner `Arc` — share one client. tower-http `ServeDir` needs a filesystem (wrong for a single-binary image). rust-embed embeds `ui/dist` at compile time. serde_yml is a deprecated shim onto **noyalib** `compat-serde-yaml`. serde-saphyr is typed-only (no `Value` tree). figment YAML+env merge cannot preserve custom knob names, `deny_unknown_fields`, or the `nodes:` reject. reqwest-middleware retries skip non-cloneable bodies but still would sit on the wrong layer for WLC node exclusion. Docker Compose `include` is the documented way to share services. GitHub composite actions DRY steps; reusable workflows are for whole jobs.

## Goals / Non-Goals

**Goals:**

- One rustls client constructor and one 429/`Retry-After` parser for router-family crates; node-agent uses a matching local helper (it must not depend on `ollama-router-core`).
- UI assets served from an embedded `ui/dist` tree; Vite may hash names without a Rust edit.
- `Authorization` marked sensitive on both Axum apps via tower-http.
- YAML `Value` merge behavior preserved on a maintained parser.
- Single Dockerfile targets + Bake file; Compose observability fragment; GHA composite + Dependabot coverage for npm/docker extras.

**Non-Goals:**

- Do not rewrite `rank_nodes`, job orchestrator, or `/api/pull` placement class.
- Do not add a new workspace crate for 20 lines of tracing init.
- Do not enable tower-http `CompressionLayer` or `TimeoutLayer` on the proxy (SSE/NDJSON / long generate).
- Do not replace the proxy's `retry_on_status` + attempted-node exclusion with HTTP middleware.
- Do not add a `mime_guess` crate (console only needs html/js/css).

## Decisions

1. **Shared rustls client in `ollama-router-core::http_util`, not reqwest-middleware.**  
   `rustls_client(connect: Option<Duration>, request: Option<Duration>) -> Result<reqwest::Client, reqwest::Error>` always calls `use_rustls_tls()` and never falls back to `Client::new()`. Router `build_upstream_client`, Verda/RunPod clients and managers, capacity/job tests, and `main.rs` CLI client all use it. **Rejected:** reqwest-middleware + `RetryTransientMiddleware` (Context7: retries need cloneable bodies; proxy retry is per-node WLC, not transport). **Rejected:** `Client::new()` fallback in Verda/RunPod managers (hides builder failures).

2. **Shared `retry_after_seconds(headers) -> f64` in `http_util`.**  
   Parse `Retry-After` then IETF `RateLimit` `reset=` (RunPod already does this); cap at 60s; default 5s. Verda and RunPod call it. **Rejected:** `backon` / `governor` (need header-driven wait, not exponential jitter on inference).

3. **Tracing init stays two thin wrappers.**  
   Router default filter `ollama_router=info`; agent `ollama_node_agent=info`. Same `fmt().json().flatten_event(true).with_env_filter(...)`. Node-agent must not take a dependency on core (would pull rusqlite/fleet). **Rejected:** new `ollama-tracing` crate; stuffing tracing-subscriber into `ollama-capacity-types`.

4. **tower-http `sensitive-headers` on both Axum apps.**  
   Enable workspace feature `sensitive-headers`. Layer `SetSensitiveRequestHeadersLayer::new([AUTHORIZATION])` next to existing `SetRequestIdLayer` / `TraceLayer` (Axum 0.8 / tower-http 0.7). **Rejected:** `CompressionLayer` (would wrap streams). **Rejected:** `TimeoutLayer` on the whole router (kills long generate). **Rejected:** `ServeDir` (needs files on disk; Docker image is a single binary).

5. **rust-embed for `/router/ui`, not hardcoded `include_bytes!`.**  
   `#[derive(Embed)] #[folder = "ui/dist"]` (relative to `crates/ollama-router`, i.e. `CARGO_MANIFEST_DIR`). Serve `index.html` and `get(path)` with a tiny MIME match on `html`/`js`/`css` (no `mime_guess` crate). 404 for unknown paths. Keep Vite `base: '/router/ui/'`. Stable `entryFileNames` may remain but must not be required by Rust. **Rejected:** tower-http `ServeDir` in production. **Rejected:** TanStack Query (console is one file + 5s `fetch` poll).

6. **noyalib `compat-serde-yaml` replaces serde_yaml 0.9; keep `Value` merge.**  
   `use noyalib::compat::serde_yaml as serde_yaml` (or equivalent) so `deep_merge`, `deny_unknown_fields`, and fleet.yaml load stay. If `cargo deny` or API mismatch blocks apply, **keep serde_yaml 0.9** and note the deferral — do not rewrite merge. **Rejected:** figment (env split `_` does not match `OLLAMA_ROUTER_*` knobs; no first-class `nodes:` reject). **Rejected:** serde-saphyr as the only parser (typed-only; no `Value` for overlay merge). **Rejected:** serde_yml crate (deprecated shim).

7. **One Dockerfile, three runtime targets; Bake for CI/publish.**  
   Keep cargo-chef pin `0.1.78` on `rust:1.97.1-slim-bookworm` (do not float `lukemathwalker/cargo-chef:latest`). `builder` compiles `ollama-router`, `ollama-mock`, and `ollama-node-agent`. Stages `router` / `mock` / `agent` copy the matching binary. **Delete `Dockerfile.agent`**; the supported command is `docker build -f Dockerfile --target agent` (Bake target `agent`). `docker-bake.hcl` is the source of truth for targets `router`, `mock`, `agent` with GHA cache scope `ollama-router`. CI (`ci.yml` docker job) and GHCR (`docker.yml`) use **`docker/bake-action`** with target **`router` only**; healthz still loads that image. debian slim + curl HEALTHCHECK stays. **Rejected:** distroless (no curl). **Rejected:** keeping a wrapper `Dockerfile.agent`. **Rejected:** `docker/build-push-action` once Bake exists (duplicate target config).

8. **Compose `include` for the observability quartet only.**  
   New `deploy/observability/compose.stack.yaml`: alertmanager, loki, alloy, grafana + `grafana-data`. Both compose files `include` it. **Prometheus stays in each file** (host scrape + `extra_hosts` vs mock scrape of `router`; different `prometheus.yml`). **Rejected:** merging Prometheus configs (would scrape the wrong target).

9. **GHA composite for rust-toolchain + rust-cache only.**  
   `.github/actions/setup-rust/action.yml` inputs: `components`, `targets`, `cache-key`. Checkout stays in each job (`persist-credentials: false`) because release-agent sometimes installs apt **before** checkout. `ci.yml` rust job: one `taiki-e/install-action@v2` with `tool: cargo-deny,cargo-llvm-cov`. Pin version **tags**, never SHAs. Dependabot: `npm` at `crates/ollama-router/ui`; extra `docker` directory for the agent-release Dockerfile if it remains a separate file. **Rejected:** reusable workflow for the whole CI job (overkill; composite is the documented DRY for steps). **Rejected:** cargo-nextest (coverage stays `cargo llvm-cov`).

## Risks / Trade-offs

- [noyalib YAML incompat with 0.9 `Value` / `Number`] → Apply-time: round-trip existing `router.defaults.yaml`, fleet fixtures, and `deny_unknown_fields` tests. If anything fails, leave serde_yaml 0.9 and document; do not weaken unknown-field rejection.
- [rust-embed empty `ui/dist` in a clean checkout] → Keep committed `ui/dist` (already in tree) or fail the router compile with a clear error; do not fetch npm in the Rust build.
- [Bake/CI cache miss on first fold of agent into chef cook] → Accept one cold cook; keep `scope=ollama-router`.
- [Compose include path/project_directory mistakes] → `docker compose -f deploy/compose.yaml config --quiet` and the mock file in tasks.
- [sensitive-headers still logs URI] → Layer only marks header values; existing `reqwest_error_for_log` / no-body rules stay.

## Migration Plan

- Land as a single PR; no fleet.yaml or tunables schema change if YAML is drop-in.
- Operators: `task docker` / `docker buildx bake router` (or `docker build -f Dockerfile --target agent` for the agent image). `docker build -f Dockerfile.agent` is removed.
- Rollback: revert the PR (images and compose go back to duplicated files).

## Open Questions

_(none — YAML fallback is a recorded risk, not a product fork.)_
