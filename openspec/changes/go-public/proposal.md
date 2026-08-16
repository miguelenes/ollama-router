## Why

The project has mature, spec-covered behavior (honest-fleet proxy, utilization WLC, Verda+RunPod autoscale) but no public presence: documentation lives in the README and internal wiki notes, there is no browsable API reference, the README badge claims "proprietary" while no `LICENSE` file exists, and there is no contribution path. As the repo prepares to go public, first-time visitors need a discoverable, branded site that explains the product and its API contract without requiring a checkout.

## What Changes

- New **Astro Starlight** site published to **GitHub Pages** (deploy via a GitHub Actions workflow, Pages source = Actions): product overview, quick start, installation (router + node-agent), `fleet.yaml` inventory guide, node-agent guide, Verda + RunPod cloud guide, an honest-fleet contract explainer, and troubleshooting/status-code reference.
- **API reference, both shapes**: hand-written reference pages for the Ollama-compatible surface (`/api/*` and OpenAI `/v1/*`, including the `/api/embeddings` → `/api/embed` rewrite and `/api/tags` union semantics) plus an **OpenAPI document** rendered in the site for the admin API (`/router/v1/*`, including enroll, drain/undrain, jobs/cancel, capacity).
- **Branding**: reuse and extend the existing `docs/assets/mark.svg` and `docs/assets/banner.svg`, Starlight theme tokens, and social/OG preview images so the site matches the README identity.
- **Repo readiness**: add a `LICENSE` file (the final license is an owner decision — the plan gates publishing on it, and lists options rather than choosing), `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, GitHub issue templates (bug / feature / docs) and a pull-request template. `SECURITY.md` already exists and stays.
- Site content must document the honest-fleet contract **faithfully**: `list` = union of holders, `infer` = holders-only, `pull` = placement job streaming NDJSON, miss = 503 `model_missing`, create/copy/push/blobs = 501, `auto_pull_on_miss` default false, unknown VRAM ≠ 0, cloud URLs tunnel/loopback-only with `public_url_blocked` on public tunnels.
- **Non-goals:** Thunder (any mention, route, or fixture); making public tunnels healthy; changing any proxy contract, route, or spec requirement; choosing the actual license text (decision gate only); flipping repo visibility itself (an owner GitHub settings action, not code); analytics/cookies or a separate marketing domain; blog/newsletter; i18n.

## Capabilities

### New Capabilities

- `public-docs-site`: the public documentation site — Starlight build, required content coverage (product, install, fleet, agent, cloud, API reference hand-written + OpenAPI), branding, GitHub Pages deployment, and the repo-readiness files (license gate, contributing, code of conduct, issue/PR templates).

### Modified Capabilities

- (none — no existing spec requirement changes; the site documents existing behavior without changing it)

## Impact

- New `site/` Astro Starlight project (npm/Node toolchain, separate from the Cargo workspace) and a new `.github/workflows/pages.yml` deploy workflow. No existing cargo CI gates change; Pages settings (source = Actions) is an owner step.
- Repo root additions: `LICENSE` (owner decision gate), `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `.github/ISSUE_TEMPLATE/`, `.github/PULL_REQUEST_TEMPLATE.md`.
- One OpenAPI document for `/router/v1/*` rendered inside the site.
- `opencode-rag.json`: exclude site build artifacts (`site/node_modules`, `site/dist`) so the walker does not index generated output; existing wiki exclusions stay untouched.
- No Rust crates, `Cargo.toml`/`Cargo.lock`, `deploy/` YAML, or proxy tests are touched.
- Sensitivity: site content must never include live tokens, secrets, zrok share tokens, SSH keys, `RUNPOD_API_KEY`, or real endpoint examples; admin-bearer docs show env placeholders only.
