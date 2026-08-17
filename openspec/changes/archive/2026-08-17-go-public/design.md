## Context

The repository (`miguelenes/ollama-router`, currently private) has a rich README, `SECURITY.md`, brand assets under `docs/assets/` (`mark.svg`, `banner.svg`) referenced by the README, and an OpenSpec tree whose main specs describe shipped behavior. There is **no `LICENSE` file** (the README badge claims "proprietary"), no contribution guide, and no browsable docs site. The only existing npm tree is the Vite console under `crates/ollama-router/ui` — unrelated lifecycle, must not be entangled with the site. CI (`ci.yml`) runs cargo gates directly and must stay unchanged. See proposal.md for motivation.

## Goals / Non-Goals

**Goals:**

- One Astro Starlight site under `site/` that builds statically and deploys to GitHub Pages from the default branch via Actions, with PR-time build+lint.
- A validated OpenAPI document for `/router/v1/*` rendered inside the site.
- Repo-readiness files committed, with the license choice an explicit **owner decision gate**.
- Content that states shipped behavior exactly (honest-fleet contract, unknown VRAM ≠ 0, tunnel/loopback-only cloud URLs) with placeholder-only examples.

**Non-Goals:**

- Flipping repository visibility (owner GitHub settings action).
- Choosing the license text (the design lists options; the owner picks).
- Rewriting the README beyond adding a link to the site; moving brand assets out of `docs/assets/`.
- Analytics, cookies, i18n, blog, search backend services, custom domain (one config variable is prepared, nothing more).
- Changing any Rust code, proxy behavior, `deploy/` YAML, or the cargo CI gates.

## Decisions

1. **Astro Starlight, npm, Node 20, standalone `site/` package.**
   Own `package.json` + committed lockfile; no dependency on the console UI tree and no Cargo integration. Alternatives rejected: VitePress and Docusaurus (Starlight's docs-first defaults — sidebar/nav/MDX, built-in theming hooks for the existing brand, and trivial static output — fit a docs + API-reference site with less custom work); mdBook (Rust-native but weak theming and no good OpenAPI-rendering story for a "modern stack" ask). Dependabot gains an npm ecosystem entry for `site/package.json`.

2. **Directory and asset layout.**
   Site content lives in `site/src/content/docs/`, API reference pages under a `reference/` group, and `openapi.yaml` at `site/openapi/openapi.yaml`. `docs/assets/` stays the canonical brand store for the README; the site commits copies into `site/src/assets/` because a git symlink would break on Windows checkouts and a build-time fetch couples the build to file moves. Drift risk is recorded under Risks.

3. **GitHub Pages via Actions, `base` prepared for project pages.**
   One workflow `pages.yml`: a `build` job (npm ci, Starlight build, `@redocly/cli lint openapi.yaml`, secret-pattern grep) runs on PRs; a `deploy` job (`actions/configure-pages`, `actions/upload-pages-artifact`, `actions/deploy-pages`, `permissions: pages: write, id-token: write`) runs only on default-branch pushes. Starlight `base` is set to `/ollama-router/` in a single config spot so a later custom domain is a one-line change. All actions pinned to version tags (repo preference; never commit SHAs). Pages source = Actions is an owner settings step called out in tasks.

4. **OpenAPI: author once, validate in CI, render statically.**
   A hand-maintained OpenAPI 3.1 `openapi.yaml` covers `/router/v1/*` (enroll, nodes, drain/undrain, jobs + cancel, capacity, admin bearer). It is linted in CI with `@redocly/cli`. Rendering uses a client-side OpenAPI viewer (RapiDoc or Swagger UI) vendored into the static build — the exact Starlight integration is chosen during apply after fetching current Starlight docs (Context7), since plugin APIs drift; the spec file itself is the contract, so the viewer can be swapped without touching specs.

5. **Repo-readiness files, license as owner gate.**
   `LICENSE` content is an owner decision: options to present are (a) proprietary all-rights-reserved text matching the existing badge, (b) source-available license (e.g., BUSL-1.1 or FCL-1.0-MIT), (c) permissive OSS (MIT or Apache-2.0). The tasks carry an owner-gate step; no other task depends on the outcome, and the final go-public checklist is blocked until the file is committed. `CONTRIBUTING.md` points at the existing `task check` / `task coverage` gates and Rust rules (no new dev process). `CODE_OF_CONDUCT.md` uses Contributor Covenant 2.1. Issue forms: bug, feature, docs (`config.yml` + three `*.yml` forms); PR template referencing the CI gates.

6. **Content is a human-facing rewrite, not a wiki dump.**
   Source material: README, `.opencode/wiki/concepts/` (internal agent notes — read for facts, never pasted), and the OpenSpec main specs (authoritative for behavior). Docs mirror shipped behavior: union list, holders-only inference, placement pull, 503 `model_missing`, 501 mutates, `auto_pull_on_miss` default false, unknown VRAM ≠ 0, `public_url_blocked`, Verda + RunPod only (no other provider named). Examples use env placeholders; no live tokens anywhere.

7. **Indexing hygiene: two `excludeDirs` entries only.**
   Add `site/node_modules` and `site/dist` to `opencode-rag.json` `excludeDirs`. No new `includeExtensions` — `.astro`/`.mdx` files stay unindexed, which is intended (docs prose is not code context). `.opencode/wiki` exclusions are untouched.

8. **Local developer loop via Taskfile passthrough.**
   Add `site:dev` and `site:build` tasks to `Taskfile.yml` that call `npm --prefix site run dev|build`. This stays consistent with "local recipes live in Taskfile" without putting cargo/docker steps into npm scripts.

## Risks / Trade-offs

- [Brand asset drift between `docs/assets/` and `site/src/assets/`] → CONTRIBUTING notes both copies must be updated together; a later change can unify the store.
- [Starlight/plugin API drift between minors] → exact versions pinned in the site lockfile; apply fetches current Starlight docs before wiring the OpenAPI viewer.
- [Wrong `base` path breaks assets on project pages] → `base` set in one config spot; the PR build job surfaces breakage before deploy.
- [Docs go stale relative to shipped behavior] → tasks include a documented-behavior checklist cross-checked against `openspec/specs` (api-tags, api-show, inference-routing, etc.); a reviewer signs off content fidelity.
- [Secret leakage through examples] → placeholder-only convention in specs; secret-pattern grep gate runs on every site workflow build.
- [Pages deployment tied to default branch only] → intentional; failed deploys leave the previous site live (Actions Pages artifact behavior).

## Migration Plan

None — greenfield static site; nothing to migrate. Rollback is a revert commit; the last successful Pages deployment remains served until a new one succeeds.

## Open Questions

- Custom domain (deferred; one config variable is prepared).
- Versioned docs for future releases (deferred until releases exist).
