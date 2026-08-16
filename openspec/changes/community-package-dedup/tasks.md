## 1. Workspace deps and YAML parser

- [ ] 1.1 Add workspace deps: `noyalib` (compat-serde-yaml), `rust-embed`; enable tower-http feature `sensitive-headers`. Do not add `mime_guess`, figment, serde-saphyr-as-sole-parser, reqwest-middleware, openssl, or native-tls.
- [ ] 1.2 Point `ollama-router-core` and `ollama-node-agent` YAML call sites at `noyalib::compat::serde_yaml` (alias `serde_yaml`) so `Value` deep-merge, `deny_unknown_fields`, and `nodes:` reject stay. If `cargo deny` or existing config/fleet tests fail, revert this crate only and keep `serde_yaml` 0.9 with a PR note — do not rewrite merge or weaken unknown-field rejection.

## 2. Shared HTTP helpers (router family)

- [ ] 2.1 Add `rustls_client` and `retry_after_seconds` to `ollama-router-core::http_util` (rustls only; never `Client::new()` fallback; Retry-After then RateLimit `reset=`; cap 60s; default 5s). Unit-test header parsing without logging URLs or bodies.
- [ ] 2.2 Switch `build_upstream_client`, Verda/RunPod clients and managers, capacity/job tests, and router CLI client construction to `rustls_client`. Keep proxy `retry_on_status` / attempted-node exclusion unchanged (no middleware retry on streams).
- [ ] 2.3 Add a matching local rustls helper in `ollama-node-agent` (setup/collect/register). Do not depend on `ollama-router-core`.

## 3. Tracing, sensitive headers, console embed

- [ ] 3.1 Leave JSON tracing as two identical wrappers that differ only by default filter (`ollama_router=info` vs `ollama_node_agent=info`). Do not extract a tracing crate or depend on core from the agent.
- [ ] 3.2 Apply `SetSensitiveRequestHeadersLayer` for `Authorization` on router and node-agent Axum apps next to existing request-id/trace layers. Do not add CompressionLayer or TimeoutLayer on the proxy.
- [ ] 3.3 Serve `/router/ui` from rust-embed of `ui/dist` (index + hashed or stable assets, tiny MIME match on html/js/css, 404 unknown). Stop hard-coding `app.js` / `index.css` in Rust. Keep Vite `base: '/router/ui/'`. Add a proxy/http test that `/router/ui` is HTML and a missing asset is 404.

## 4. Docker Bake and Compose include

- [ ] 4.1 Fold agent into root `Dockerfile` as target `agent` sharing cargo-chef 0.1.78 / rust 1.97.1-slim-bookworm; builder builds router, mock, and node-agent. Keep debian slim + curl HEALTHCHECK. **Delete `Dockerfile.agent`**; retarget comments (e.g. `config.docker.yaml`) to `--target agent`.
- [ ] 4.2 Add `docker-bake.hcl` targets `router`, `mock`, `agent` with GHA cache scope `ollama-router`. Switch `ci.yml` docker job and `docker.yml` GHCR publish to **`docker/bake-action`** with target `router` only; healthz still checks that image. Do not install `task` in GitHub Actions.
- [ ] 4.3 Extract Grafana/Alloy/Loki/Alertmanager + `grafana-data` into `deploy/observability/compose.stack.yaml` and `include` it from `deploy/compose.yaml` and `deploy/compose.mock.yaml`. Leave Prometheus in each file (host vs mock scrape). Validate with `docker compose -f deploy/compose.yaml config --quiet` and the mock file.

## 5. GitHub Actions and Dependabot

- [ ] 5.1 Add `.github/actions/setup-rust/action.yml` (toolchain 1.97, optional components/targets/cache-key). Use it from `ci.yml`, `codeql.yml`, and `release-agent.yml` rust jobs. Keep checkout in each job with `persist-credentials: false`. Version tags only, never SHAs.
- [ ] 5.2 Combine cargo-deny and cargo-llvm-cov into one `taiki-e/install-action@v2` `tool:` list in `ci.yml`.
- [ ] 5.3 Extend Dependabot: `npm` directory `crates/ollama-router/ui`; extra `docker` directory for `crates/ollama-node-agent/packaging/linux` if that Dockerfile remains. Do not add npm scripts, a Makefile, or a justfile.

## 6. Docs and gates

- [ ] 6.1 Update AGENTS.md / README / Taskfile docker notes for `--target agent`, Bake, Compose include, and the removed `Dockerfile.agent`. Mention rust-embed UI serving if those docs list `app.js`.
- [ ] 6.2 Run `task check` (fmt, clippy `-D warnings`, test `--locked`, deny) and `task coverage` (line coverage ≥80%, ignore `**/main.rs` only). Fix failures without lowering the floor or excluding crates.
