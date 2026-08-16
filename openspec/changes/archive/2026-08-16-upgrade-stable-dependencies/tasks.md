## 1. Inventory and target selection

- [x] 1.1 Record current pins (rust-version, Docker bases, Compose images, GHA actions, NFPM, UI packages) into the PR notes as before/after.
- [x] 1.2 Resolve newest stable targets: rustc + matching `rust:*-slim-*`, `debian:*-slim`, each workspace crate major/minor, Compose image tags, GHA action majors, NFPM release, React/Vite/TypeScript versions (no RC/beta/`latest` floats in committed files).

## 2. Rust toolchain and Docker bases

- [x] 2.1 Bump workspace `rust-version` (and edition only if a crate requires it) in root `Cargo.toml`.
- [x] 2.2 Update `Dockerfile`, `Dockerfile.agent`, `crates/ollama-node-agent/packaging/linux/Dockerfile.release`, and GHA `container:` / `toolchain:` / `dtolnay/rust-toolchain` pins to the same rust + Debian slim tags (match workspace `rust-version`).
- [x] 2.3 Confirm local `rustc`/`rustup` matches the new pin before compiling.

## 3. Cargo workspace upgrades

- [x] 3.1 Bump `[workspace.dependencies]` caret/exact ranges toward newest stable; run `cargo update` and commit `Cargo.lock`.
- [x] 3.2 For each breaking HTTP/async major (Axum, Tokio, reqwest, tower-http, etc.), fetch current docs (Context7/docs.rs) and migrate call sites while preserving streaming proxy behavior and rustls-only TLS; if a major has no GA migration path, pin the newest stable minor on the current major and record the deferral in the PR notes.
- [x] 3.3 Fix remaining compile/clippy fallout across all workspace crates (no new `unwrap`/`expect` in non-test lib code; no openssl/native-tls).
- [x] 3.4 Resolve any new `cargo deny` advisories without weakening the openssl/native-tls ban list.

## 4. GitHub Actions and release tooling

- [x] 4.1 Bump actions in `ci.yml`, `docker.yml`, `release-agent.yml`, `codeql.yml`, and `dependency-review.yml` to newest stable version tags (never SHAs); adjust inputs per release notes.
- [x] 4.2 Bump `NFPM_VERSION` (and any other pinned release-tool versions) to newest stable; keep GHA free of Task installs.

## 5. Compose observability stack

- [x] 5.1 Bump Prometheus, Alertmanager, Loki, Grafana Alloy, and Grafana image tags in `deploy/compose.yaml` and `deploy/compose.mock.yaml`.
- [x] 5.2 Update Alloy config and stack dashboard captions if the new Alloy/Grafana versions require it; validate `docker compose config`.
- [x] 5.3 Smoke scrapes (router only — never node-agent `:11436`) and confirm home dashboard UID `ollama-router` still loads.

## 6. Console UI packages

- [x] 6.1 Replace `"latest"` in `crates/ollama-router/ui/package.json` with concrete stable semver pins for React, Vite, TypeScript, plugins, and `@types/*`.
- [x] 6.2 Install/refresh the lockfile if the package manager produces one; run the UI `lint`/`build` scripts successfully.

## 7. Docs and pin references

- [x] 7.1 Update AGENTS.md, `.cursor/rules/rust.mdc`, packaging wiki, and skills that hard-code old rustc/Docker tags to the new pins.
- [x] 7.2 Note glibc floor / Debian suite in packaging docs if the slim base suite changed.

## 8. Gates

- [x] 8.1 Run `task check` (fmt, clippy `-D warnings`, test `--locked`, deny) and fix failures.
- [x] 8.2 Run `task coverage` and keep line coverage ≥ 80% (ignore `**/main.rs` only).
- [x] 8.3 Run `task docker` (or equivalent image build) to confirm multi-stage builds succeed on the new bases.
