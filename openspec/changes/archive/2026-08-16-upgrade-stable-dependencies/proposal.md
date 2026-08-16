## Why

Pinned toolchain, crates, container images, Compose observability stack, GitHub Actions, and the console UI packages have drifted behind current stable releases. Bringing them current reduces known CVEs and unblocks API fixes, while keeping the honest-fleet Ollama proxy contract unchanged.

## What Changes

- Bump workspace Cargo dependencies (`Cargo.toml` / `Cargo.lock`) to the latest **stable** crates.io releases compatible with each other; fix **BREAKING** API churn (e.g. Axum/Tokio/reqwest/rusqlite majors) via docs (Context7 / docs.rs), not pin freezes.
- Raise the workspace `rust-version` pin and matching Docker/GHA images (`rust:*-slim-*`) to the newest stable rustc the ecosystem supports; keep edition **2021** unless a stable edition migration is required by a bumped crate.
- Refresh multi-stage Docker bases (`debian:*-slim`, agent release Dockerfile) to the newest stable Debian slim tags that still match packaging assumptions (glibc floor for `.deb`).
- Bump Compose observability images in `deploy/compose.yaml` / `compose.mock.yaml` (Prometheus, Alertmanager, Loki, Grafana Alloy, Grafana) to newest stable tags; adjust Alloy config / dashboards if **BREAKING**.
- Bump GitHub Actions to newest stable major tags (`actions/*`, `docker/*`, `Swatinem/rust-cache`, `taiki-e/install-action`, CodeQL, attest, softprops, etc.) — version tags only, never commit SHAs.
- Pin or refresh `NFPM_VERSION` and other release-tool versions in `release-agent.yml` / packaging scripts to newest stable.
- Replace console UI `package.json` `"latest"` floats with concrete stable semver pins (React, Vite, TypeScript, `@types/*`) and refresh lockfile if present.
- Update AGENTS.md / wiki / skills / rules that hard-code rustc or image tags so docs match the new pins.
- Re-run `task check` and `task coverage` (≥80% lines); resolve `cargo deny` advisories introduced by bumps without weakening openssl/native-tls bans.

**Non-goals:** Thunder; public tunnels; changing idle/enroll/WLC product behavior; adding HA/Redis; scraping node-agent `:11436`; inventing new router APIs; upgrading host Ollama itself beyond what CI/docs mention; third-party zrok compose under `.local/zrok`; Dependabot/renovate automation; Verda/RunPod GPU marketplace image strings in tunables (unless a pin is abandoned at apply time); `.opencode` local npm tooling (not a shipped app).

## Capabilities

### New Capabilities

_(none — `skip_specs: true`; this change does not alter product requirements.)_

### Modified Capabilities

_(none)_

## Impact

- **Crates:** entire workspace (`ollama-router`, `ollama-router-core`, `ollama-router-verda`, `ollama-router-runpod`, `ollama-node-agent`, `ollama-mock`, `ollama-capacity-types`) via `Cargo.toml` / `Cargo.lock`.
- **Containers:** `Dockerfile`, `Dockerfile.agent`, `crates/ollama-node-agent/packaging/linux/Dockerfile.release`, GHA `container:` images.
- **Compose:** `deploy/compose.yaml`, `deploy/compose.mock.yaml`, possibly Alloy config + stack dashboard descriptions.
- **CI:** `.github/workflows/{ci,docker,release-agent,codeql,dependency-review}.yml`.
- **UI:** `crates/ollama-router/ui/package.json` (+ lockfile if added).
- **Docs:** AGENTS.md, `.cursor/rules/rust.mdc`, packaging wiki, skills that cite `rust:1.97`.
- **Gates:** `task check`, `task coverage`, `cargo deny`; compose config validation.
- Honest-fleet contract (list = union, infer = holders, pull = placement, miss = 503) is **preserved**.
