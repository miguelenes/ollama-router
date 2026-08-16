## Why

Operators and maintainers currently copy the same rustls client, JSON tracing, 429 Retry-After handling, cargo-chef Docker stages, Compose observability services, and GitHub rust-setup steps in many places. Those copies drift (agent image vs router rustc, Grafana tags in `compose.yaml` vs `compose.mock.yaml`), the console hard-codes Vite asset names, and HTTP traces can treat the admin bearer like any other header. Adopting a few community packages and shared helpers closes that gap without changing the honest-fleet proxy contract.

## What Changes

- Extract a workspace rustls `reqwest::Client` factory and a shared 429/`Retry-After` **parser** (`retry_after_seconds`) in `ollama-router-core` `http_util` (reqwest 0.13 already clones cheaply; reuse one client instead of ~15 `Client::builder().use_rustls_tls()` sites). Callers still sleep; do **not** put `reqwest-middleware` on the inference proxy (streaming bodies and node-exclusion retry are product logic).
- Keep matching JSON `tracing_subscriber` wrappers in the router and node-agent binaries (default filter only; no shared crate — node-agent must not depend on core). Add tower-http `sensitive-headers` so `Authorization` is marked sensitive on both Axum apps (admin bearer must not appear in traces).
- Replace hardcoded `include_bytes!("…/ui/dist/assets/app.js")` (and CSS) with **rust-embed** of `ui/dist` so `/router/ui` serves whatever Vite emits without pinning filenames in Rust. MIME is a tiny `html`/`js`/`css` match (no `mime_guess` crate).
- Keep tunables YAML as `Value` deep-merge + `deny_unknown_fields` + custom `OLLAMA_ROUTER_*` knobs. Replace unmaintained `serde_yaml` 0.9 with the maintained **noyalib** `compat-serde-yaml` surface (serde_yml is only a deprecated shim). Do **not** switch the merge path to figment or typed-only serde-saphyr.
- Deduplicate Docker: one `Dockerfile` with `router` / `mock` / `agent` targets sharing the cargo-chef planner/builder; **delete `Dockerfile.agent`**. Add `docker-bake.hcl` as the source of truth; CI load and GHCR push use **`docker/bake-action`** with target `router`. Keep debian slim + curl HEALTHCHECK (no distroless).
- Deduplicate Compose: `include` a shared observability fragment from both `deploy/compose.yaml` and `deploy/compose.mock.yaml` (same pattern as `compose.zrok.yaml`).
- Deduplicate GitHub Actions: a local composite action for **rust-toolchain 1.97 + rust-cache only** (checkout stays in each job with `persist-credentials: false`); one `taiki-e/install-action` tool list; Dependabot entries for the console npm tree and extra Dockerfiles. Version tags only (never commit SHAs).

**Non-goals:** Thunder; public tunnels; native Hub-pull through one node; default `auto_pull_on_miss`; 404-on-miss; agent-down unhealthy; Redis/HA replicas; Prometheus scrape of node-agent `:11436`; OpenTelemetry/Tempo; swapping `prometheus` for `metrics`; sqlx; figment/`config` crate; `mime_guess`; reqwest-middleware or CompressionLayer/TimeoutLayer on generate/chat/embed streams; cargo-nextest; TanStack Query or other SPA frameworks; bumping rustc/crate majors (that was `upgrade-stable-dependencies`).

## Capabilities

### New Capabilities

_(none — `skip_specs: true`; this change does not alter product requirements.)_

### Modified Capabilities

_(none)_

## Impact

- **Crates:** `ollama-router-core` (`http_util`, YAML parser), `ollama-router` (proxy client, tracing, UI embed, tower-http layers), `ollama-router-verda` / `ollama-router-runpod` (shared rustls client + 429 helper), `ollama-node-agent` (tracing, rustls downloads, tower-http), tests that construct `reqwest::Client`.
- **UI:** `crates/ollama-router/ui` stays React+Vite; Rust no longer lists asset filenames. `base` remains `/router/ui/`.
- **Containers:** `Dockerfile` (`router` / `mock` / `agent` stages), delete `Dockerfile.agent`, `docker-bake.hcl`, CI/GHCR `docker/bake-action` (router target).
- **Compose:** `deploy/compose.yaml`, `deploy/compose.mock.yaml`, new observability include file; mock Prometheus scrape file stays distinct.
- **CI:** `.github/workflows/{ci,docker,release-agent,codeql}.yml`, `.github/actions/*`, `.github/dependabot.yml` (npm + extra docker directories).
- **Gates:** `task check`, `task coverage` (≥80% lines), `task docker`, `docker compose config`. Sensitivity: no bodies/tokens in logs; `Authorization` marked sensitive. Honest-fleet (list = union, infer = holders, pull = placement, miss = 503, 501 mutates) is **preserved**. Ranking / unknown-VRAM vs measured CPU is **unchanged**.
