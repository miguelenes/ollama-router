## 1. Site scaffold

- [ ] 1.1 Fetch current Astro Starlight docs (Context7) for scaffolding and theming before writing any site code; scaffold the site in `site/` with Node 20, npm, its own `package.json` and a committed lockfile (separate from the console UI npm tree)
- [ ] 1.2 Configure `astro.config` with `base: '/ollama-router/'`, site title, and nav groups (Guide, Reference, Admin API, FAQ) (spec: Site is published to GitHub Pages)
- [ ] 1.3 Copy `docs/assets/mark.svg` and `docs/assets/banner.svg` into `site/src/assets/`, wire the mark as logo/favicon, and generate OG/social preview images from the banner (spec: Branding reuses the existing identity)
- [ ] 1.4 Build the homepage: banner hero, honest-fleet one-liner, feature summary (WLC routing, Verda + RunPod spot GPUs, tunnel/loopback-only cloud URLs), and the site link targets

## 2. Docs content

- [ ] 2.1 Quick start page whose commands match the shipped product (router, node-agent, fleet file) (spec: First-time visitor reaches a working quick start)
- [ ] 2.2 Installation page: router binary + Docker, node-agent `setup`/`serve` on `:11436`, no-supervision note (spec: Core documentation covers the product journey)
- [ ] 2.3 `fleet.yaml` guide: YAML overlays are tunables-only, top-level `nodes:` is a hard config error, labels, GiB = bytes / 1024³ (spec: Core documentation covers the product journey)
- [ ] 2.4 Node-agent guide: probes, pressure levels, soft-fail behavior (spec: Core documentation covers the product journey)
- [ ] 2.5 Cloud guide covering Verda and RunPod only: tunnel/loopback-only cloud URLs, `public_url_blocked` for public endpoints, no other provider named (spec: Cloud guide names only supported providers)
- [ ] 2.6 Architecture page stating every honest-fleet deviation: union list, holders-only infer, placement pull streaming NDJSON, 503 `model_missing` (not 404), 501 create/copy/push/blobs, `auto_pull_on_miss` default false, holder-only `show`, one replica / no Redis (spec: Docs state the honest-fleet contract accurately)
- [ ] 2.7 Status-code reference page: `no_healthy`, `all_nodes_saturated`, `model_missing`, `insufficient_capacity`, `public_url_blocked`, each with `Retry-After` on 503 (spec: Status-code reference covers 503 semantics)

## 3. API reference

- [ ] 3.1 Hand-written reference pages for `/api/chat`, `/api/generate`, `/api/embeddings` (states the rewrite to `/api/embed`), `/api/tags` (union across healthy nodes + placeholder digest rule), `/api/ps`, `/api/show`, `/api/version` (spec: Hand-written API reference for the Ollama-compatible surface)
- [ ] 3.2 Hand-written reference pages for the OpenAI-compatible `/v1/chat/completions` and `/v1/embeddings`, with the same 503 reason-code coverage (spec: Hand-written API reference for the Ollama-compatible surface)
- [ ] 3.3 Author `site/openapi/openapi.yaml` (OpenAPI 3.1) covering `/router/v1/*`: enroll, nodes, drain/undrain, jobs and job cancel, capacity, and the admin bearer with fail-closed text (unset `OLLAMA_ROUTER_ADMIN_TOKEN` → 403) using env placeholders only (spec: Admin API reference is an OpenAPI document rendered in the site)
- [ ] 3.4 Render the OpenAPI document in the site navigation with a vendored client-side viewer (RapiDoc or Swagger UI — pick the integration from the Starlight docs fetched in 1.1) (spec: OpenAPI document validates and renders)
- [ ] 3.5 Add `@redocly/cli lint` as a site script and confirm `openapi.yaml` validates cleanly (spec: OpenAPI document validates and renders)

## 4. Repo readiness

- [ ] 4.1 OWNER GATE: present license options (proprietary all-rights-reserved text matching the existing badge; source-available BUSL-1.1 or FCL-1.0-MIT; permissive MIT or Apache-2.0), record the owner's decision, and commit the chosen `LICENSE` file (spec: License decision gates the go-public step)
- [ ] 4.2 Add `CONTRIBUTING.md`: `task check` / `task coverage` gates, sensitivity rules, and the note that `docs/assets/` and `site/src/assets/` brand copies must be updated together (spec: Contributor path is discoverable from the repo root)
- [ ] 4.3 Add `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) (spec: Contributor path is discoverable from the repo root)
- [ ] 4.4 Add `.github/ISSUE_TEMPLATE/` forms for bug, feature, and docs reports plus `config.yml` (spec: Contributor path is discoverable from the repo root)
- [ ] 4.5 Add `.github/PULL_REQUEST_TEMPLATE.md` referencing the CI gates (spec: Contributor path is discoverable from the repo root)
- [ ] 4.6 Add a link to the published site in the README (no README rewrite)

## 5. CI + tooling

- [ ] 5.1 Add `.github/workflows/pages.yml`: `build` job on PRs (npm ci, Starlight build, redocly lint, secret grep) and `deploy` job on default-branch pushes (`configure-pages`, `upload-pages-artifact`, `deploy-pages`) with `permissions: pages: write, id-token: write`; all actions pinned to version tags, never commit SHAs (spec: Site is published to GitHub Pages / Failed build does not replace the live site)
- [ ] 5.2 Add the secret-pattern grep gate over site source and rendered output (token patterns, `RUNPOD_API_KEY`, zrok share tokens, SSH keys) (spec: Automated scan finds no live secrets in content)
- [ ] 5.3 Add `site:dev` and `site:build` passthrough tasks to `Taskfile.yml` (npm --prefix site); no Makefile/justfile, no npm scripts driving cargo
- [ ] 5.4 Update `opencode-rag.json` `excludeDirs` with `site/node_modules` and `site/dist`; leave `.opencode/wiki` exclusions untouched (spec: Indexer skips site build artifacts)
- [ ] 5.5 Add a Dependabot npm entry for `site/package.json`

## 6. Verification

- [ ] 6.1 `npm --prefix site run build` succeeds; preview the site locally and confirm assets resolve under the `/ollama-router/` base path (spec: Push to default branch publishes the site)
- [ ] 6.2 Run the secret grep over `site/` and the OpenAPI file; zero matches (spec: Automated scan finds no live secrets in content)
- [ ] 6.3 Documented-behavior checklist: cross-check 2.6, 2.7, and the reference pages against `openspec/specs/` (api-tags, api-show, api-pull, inference-routing, model-placement, request-class, size-load-routing, cloud-autoscale, cloud-provider-runpod); fix any doc drift in this pass
- [ ] 6.4 Run `task check` (Taskfile changed) and confirm it stays green; no Rust files, crates, or `deploy/` YAML were touched
- [ ] 6.5 Record the two owner actions in the final summary (not code): set GitHub Pages source to Actions, and flip repository visibility only after the 4.1 license gate is satisfied
