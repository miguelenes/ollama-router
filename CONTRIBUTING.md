# Contributing

Thanks for contributing to **ollama-router** — a mixed CPU+GPU
Ollama-compatible fleet proxy. The router process needs no GPU; the stack is
Rust (edition 2021, rustc 1.97), Axum 0.8 + Tokio, reqwest with rustls.

## Before you start

- Open an issue first for anything beyond a small fix — behavior changes
  should be described against the [honest-fleet contract](https://miguelenes.github.io/ollama-router/guides/architecture/).
- The admin API is fail-closed (`OLLAMA_ROUTER_ADMIN_TOKEN` unset → 403).
  Never commit tokens, secrets, share tokens, SSH keys, or `RUNPOD_API_KEY`.

## Development

Local recipes live in [Taskfile.yml](Taskfile.yml) (no Makefile):

```bash
task check      # sequential: fmt --check, clippy -D warnings, test --locked, deny
task coverage   # cargo llvm-cov, ≥80% lines (ignores **/main.rs)
task dev        # host Ollama :11434 → router :11435 + agent :11436
task compose:up # Grafana :3000 / Prometheus :9090
```

The finish gate is `task check` **and** `task coverage`. Do not lower the
coverage floor, and do not add `unwrap`/`expect` in non-test library code
(libraries use `thiserror`; `anyhow` is binary-only). Never add
`native-tls`/`openssl` (rustls only — `deny.toml` bans them).

## Documentation site

The docs site lives in [`site/`](site/) (Astro Starlight + a Redocly-rendered
OpenAPI reference):

```bash
cd site
npm install
npm run lint:openapi   # validate site/openapi/openapi.yaml
npm run build:openapi  # regenerate public/openapi.html
npm run build          # static build (runs gen:og + astro build)
```

- Docs must state **shipped** behavior, not aspirations.
- Brand assets are duplicated by design: `docs/assets/` (README) and
  `site/src/assets/` (site) — update **both** copies together.

## Code review

- Stream NDJSON/SSE as it arrives; never buffer inference bodies.
- Never log request/response bodies, prompts, embeddings, share tokens,
  Verda tokens/secrets, `RUNPOD_API_KEY`, SSH keys, or the admin bearer.
- Tests: unit tests next to modules, `httpmock` for HTTP paths, no live cloud
  calls. Keep workspace line coverage ≥ 80%.

## License

Apache-2.0 (see [LICENSE](LICENSE)). All contributions are licensed under the
same terms.
