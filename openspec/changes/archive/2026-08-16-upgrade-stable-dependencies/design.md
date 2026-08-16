## Context

See proposal.md — Why. Today the repo pins rustc **1.97**, Docker `rust:1.97-slim-bookworm` → `debian:bookworm-slim`, Cargo workspace crates with caret ranges, Compose observability at Prometheus v3.2.1 / Alertmanager v0.28.1 / Loki 3.4.2 / Alloy v1.9.2 / Grafana 11.6.3, GitHub Actions on major tags (`checkout@v7`, `rust-cache@v2`, Docker actions v4/v6/v7, CodeQL `@v4`), and console UI deps floated as `"latest"`. `cargo deny` bans openssl/native-tls; coverage gate is ≥80% lines. Product specs are unchanged (`skip_specs`).

## Goals / Non-Goals

**Goals:**
- Single coordinated bump wave: resolve newest **stable** (non-RC/beta) versions per surface, apply them, fix compile/test/docs fallout.
- Prefer documented migration paths (changelogs, Context7, docs.rs) over staying on old majors when a stable major exists.
- Keep rustls-only TLS, streaming proxy semantics, and fail-closed admin/credentials.

**Non-Goals:**
- Changing routing, idle, enroll, or cloud-provider product logic except as required to compile against bumped APIs.
- Replacing Debian with Alpine, or moving packaging to musl for the router image.
- Upgrading third-party zrok compose (fetched under `.local/zrok`).
- Adding Dependabot/renovate automation in this change (optional follow-up).
- `.opencode` local npm tooling (not a shipped app).

## Decisions

1. **Definition of “most stable”**  
   - **Meaning:** newest non-prerelease release on the default channel (crates.io stable, Docker Hub stable tags, GitHub Actions major tags that are GA).  
   - **Not:** nightly rustc, `-rc` images, or floating `"latest"` left in committed manifests.  
   - **Alternative rejected:** “only security patches within current minors” — user asked for full upgrades including breaking majors.

2. **Order of work**  
   - Toolchain (rustc + Docker rust image) → Cargo workspace bump (`cargo update` / explicit major bumps) → fix compile/clippy → GHA action tags + NFPM → Compose images + Alloy/Grafana smoke → UI package pins → doc tag rewrites → gates.  
   - **Rationale:** rustc/crate majors drive most breakages; images/Actions are lower risk once the binary builds.  
   - **Alternative rejected:** bump everything in one blind PR without staged compile — too hard to attribute failures.

3. **Rust edition**  
   - Stay on **2021** unless a bumped crate **requires** a newer edition to compile; only then move workspace `edition` and update rules/docs.  
   - **Alternative rejected:** opportunistic edition 2024 migration in the same change without a crate forcing it.

4. **Debian base**  
   - Prefer newest stable `debian:*-slim` that matches the rust image’s glibc story (today bookworm; move to trixie/next only if the matching `rust:*-slim-*` tag exists and agent `.deb` glibc floor stays documented).  
   - Keep multi-stage non-root + `HEALTHCHECK` curl to `/healthz`.

5. **Cargo majors (Axum / Tokio / reqwest / tower-http / rusqlite / …)**  
   - Before coding against bumped HTTP crates, fetch current docs (Context7 + docs.rs).  
   - Preserve Axum routing shape and streaming body forward; no buffer-full-body regressions.  
   - If a major is unstable or lacks a clear migration, document the block in tasks and pin the newest stable minor on the current major — only after checking docs show no GA major yet.

6. **Compose observability**  
   - Bump image tags together; validate `docker compose config`; smoke Prometheus scrape of router `:11435` and Grafana home UID `ollama-router`.  
   - If Alloy config syntax changes, update `deploy/observability/config.alloy` and stack dashboard captions that hard-code Alloy versions.  
   - Do not add a scrape for node-agent `:11436`.

7. **GitHub Actions**  
   - Keep **version tags** (`@vN`), never SHAs.  
   - `dtolnay/rust-toolchain@stable` may stay on floating `stable` **or** pin the same rustc as `rust-version` — prefer explicit toolchain version matching workspace `rust-version` for reproducibility once the pin moves.

8. **Console UI**  
   - Replace `"latest"` with concrete stable versions resolved at apply time; add/commit a lockfile if the package manager produces one and the repo convention allows it.  
   - Stay on Vite + React SPA; no framework rewrite.

9. **RunPod/Verda default images in tunables**  
   - Out of scope unless a pin is clearly abandoned; those are cloud GPU marketplace images, not build deps. Leave `router.defaults.yaml` image strings unless apply-time research shows a required replacement for bootstrap.

## Risks / Trade-offs

- **[Risk]** Large Cargo major (e.g. reqwest 0.12→0.13, rusqlite) breaks many call sites → **Mitigation:** bump one major family at a time; use docs + compile errors; keep tests green before next family.
- **[Risk]** Grafana/Prometheus major breaks dashboard JSON or PromQL → **Mitigation:** compose up + Prometheus/Grafana MCP smoke; fix panels additively; keep home dashboard UID.
- **[Risk]** Newer Debian glibc breaks older host installs of `.deb` → **Mitigation:** document glibc floor in packaging wiki; prefer bookworm until trixie is clearly the rust image default.
- **[Risk]** Action major renames inputs → **Mitigation:** read each action’s release notes before bumping; run workflow dry via CI on the PR.
- **[Risk]** Coverage dips from deleted code paths during API cleanup → **Mitigation:** add/adjust tests; never lower the 80% floor.

## Migration Plan

1. Land on a branch; apply bumps; keep `task check` + `task coverage` green locally.  
2. CI on PR validates fmt/clippy/test/deny/cov + Docker build.  
3. After merge: rebuild local images (`task docker`), refresh compose stack (`task compose:up`), confirm scrapes.  
4. Rollback: revert the merge commit; Docker/Compose tags return with the revert (no data migration).

## Open Questions

- Exact target versions are resolved at apply time from crates.io / Docker Hub / GitHub Releases (planning does not freeze numbers that will stale overnight).
