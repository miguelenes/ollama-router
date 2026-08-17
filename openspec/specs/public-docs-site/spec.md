# public-docs-site Specification

## Purpose
The public documentation site for ollama-router: a branded GitHub Pages site with product docs, a hand-written Ollama-compatible API reference, a rendered OpenAPI document for the admin API, and the repo-readiness files required before the repository goes public.

## Requirements

### Requirement: Site is published to GitHub Pages

The site SHALL be built and deployed to GitHub Pages by the Woodpecker Pages pipeline on the repository's default branch, with GitHub Pages configured to serve the `gh-pages` branch. Publishing SHALL NOT require a manual upload or a checkout of the live site.

#### Scenario: Push to default branch publishes the site

- **WHEN** a change lands on the default branch and the Pages pipeline runs successfully
- **THEN** the site is built, `site/dist` is published to the `gh-pages` branch, and the public Pages URL serves the updated content

#### Scenario: Failed build does not replace the live site

- **WHEN** the Pages pipeline build or deploy step fails
- **THEN** the previously published site remains reachable and no partial artifact is served

### Requirement: Core documentation covers the product journey

The site SHALL document, without requiring a source checkout: the product overview, a working quick start, installation of the router and the node-agent, `fleet.yaml` inventory (including the rule that YAML overlays are tunables-only and top-level `nodes:` is a hard config error), the node-agent setup/serve model, and the Verda + RunPod cloud providers.

#### Scenario: First-time visitor reaches a working quick start

- **WHEN** a first-time visitor starts at the homepage and follows the navigation
- **THEN** they can reach a quick-start page whose commands match the shipped product (router, node-agent, fleet file), and a fleet guide that names the tunables-only constraint

#### Scenario: Cloud guide names only supported providers

- **WHEN** a reader opens the cloud autoscaling guide
- **THEN** it covers Verda and RunPod and describes tunnel/loopback-only cloud URLs with `public_url_blocked` for public endpoints, and mentions no other provider

### Requirement: Docs state the honest-fleet contract accurately

Documentation SHALL state the honest-fleet proxy contract exactly as shipped: `list` is the union of holders, inference targets only nodes that already have the model, `pull` is a placement job streaming NDJSON, a model miss returns 503 `model_missing` (not 404), create/copy/push/blobs return 501, `auto_pull_on_miss` defaults to false, and `show` returns the model from its holder only.

#### Scenario: Compatibility deviations are discoverable

- **WHEN** a reader consults the docs to compare the router against a single Ollama daemon
- **THEN** each deviation (union list, holders-only infer, placement pull, 503 `model_missing`, 501 mutates, default-false auto-pull) is stated with the router's actual behavior, not the native Ollama behavior

### Requirement: Hand-written API reference for the Ollama-compatible surface

The site SHALL provide hand-written reference pages for `/api/chat`, `/api/generate`, `/api/embeddings` (documenting the rewrite to `/api/embed`), `/api/tags` (documenting the CLI-compatible union and digest placeholder semantics), `/api/ps`, `/api/show`, `/api/version`, and the OpenAI-compatible `/v1/chat/completions` and `/v1/embeddings`. Each page SHALL document the 503 reason codes (`no_healthy`, `all_nodes_saturated`, `model_missing`, `insufficient_capacity`, `public_url_blocked`) and the `Retry-After` header on those responses.

#### Scenario: Reader looks up an endpoint with a rewrite or union behavior

- **WHEN** a reader opens the `/api/embeddings` reference page
- **THEN** the page states that requests are rewritten to `/api/embed`; and when they open `/api/tags`, the page describes the union across healthy nodes and the placeholder digest rule

#### Scenario: Status-code reference covers 503 semantics

- **WHEN** a reader consults the status-code reference
- **THEN** every 503 reason code above is listed with its meaning and the presence of `Retry-After`

### Requirement: Admin API reference is an OpenAPI document rendered in the site

The site SHALL include a machine-readable OpenAPI document covering the `/router/v1/*` admin API (enroll, nodes, drain/undrain, models ensure/delete, jobs and job cancel, stats, reload, readiness, Verda and RunPod status/ensure/destroy, and the admin bearer requirement) and SHALL render it within the site navigation. The document SHALL state that an unset `OLLAMA_ROUTER_ADMIN_TOKEN` disables the admin API with 403. Every operation the document lists SHALL correspond to a route registered by the router binary; the document SHALL NOT advertise operations the router does not serve.

#### Scenario: OpenAPI document validates and renders

- **WHEN** the OpenAPI document is validated against the OpenAPI 3 schema and the site is built
- **THEN** validation passes and the rendered reference is reachable from the site navigation

#### Scenario: Admin bearer documented fail-closed without secrets

- **WHEN** a reader opens the admin API reference
- **THEN** it explains the fail-closed behavior (unset token → 403, no default secret) and contains only env-var placeholders, never a live token

#### Scenario: Documented operations match the shipped admin routes

- **WHEN** the OpenAPI document is compared against the routes registered by the router binary
- **THEN** every documented operation maps to a registered `/router/v1/*` route and no documented operation (such as a "capacity" path) exists without a matching route

### Requirement: Branding reuses the existing identity

The site SHALL use the existing mark and banner assets as its logo and header identity, derive its theme from them, and provide social/OG preview images for link sharing.

#### Scenario: Homepage matches the repository identity

- **WHEN** a visitor opens the homepage
- **THEN** the mark appears as the site logo, the banner matches the README banner, and OG/social preview images exist for shared links

### Requirement: Repo-readiness files exist before going public

The repository SHALL contain a `LICENSE` file whose text was explicitly approved by the owner, a `CONTRIBUTING.md`, a `CODE_OF_CONDUCT.md`, GitHub issue templates for bug, feature, and docs reports, and a pull-request template. Public promotion SHALL be gated until the license decision is recorded.

#### Scenario: Contributor path is discoverable from the repo root

- **WHEN** a visitor opens the repository
- **THEN** `LICENSE`, `CONTRIBUTING.md`, and `CODE_OF_CONDUCT.md` are visible at the root, and opening a new issue offers bug, feature, and docs templates

#### Scenario: License decision gates the go-public step

- **WHEN** the change is being finalized for public promotion
- **THEN** the task list marks the license decision as an owner gate that blocks the final promotion step until an approved license file is committed

### Requirement: Site content contains no secrets

Site content SHALL NOT contain live credentials, tokens, zrok share tokens, SSH keys, `RUNPOD_API_KEY` values, or real cloud endpoint examples; configuration examples SHALL use env-var placeholders.

#### Scenario: Automated scan finds no live secrets in content

- **WHEN** a secret-scanning check runs over the site source and rendered content
- **THEN** it reports no matches for token, key, or share-token patterns

### Requirement: Site build output stays out of the repo index

Build artifacts and dependencies of the site SHALL be excluded from the workspace indexing configuration so the indexer does not walk generated output.

#### Scenario: Indexer skips site build artifacts

- **WHEN** the indexing configuration is read for the site directory
- **THEN** `site/node_modules` and `site/dist` (or the equivalent build-output directories) are excluded

### Requirement: Public promotion includes live GitHub Pages

The public documentation site SHALL be served from GitHub Pages using the `gh-pages` branch published by the Woodpecker Pages pipeline from the default branch, and the README SHALL link the public site URL (path base `/ollama-router/`). Repository visibility is already public; remaining owner GitHub settings actions are Pages source = Deploy from a branch (`gh-pages`) and confirming the live URL.

#### Scenario: Pages branch is published

- **WHEN** an operator completes the Pages promotion checklist
- **THEN** repository Pages settings serve the `gh-pages` branch and a successful `main` Pages pipeline publishes the site

#### Scenario: README points at the live site

- **WHEN** a visitor opens the repository README
- **THEN** they can follow a link to the published GitHub Pages URL for this project

#### Scenario: Visibility flip is an owner gate

- **WHEN** code and pipelines for Pages and packages are ready
- **THEN** repository visibility stays public (already flipped) and is not changed by application code; the remaining owner GitHub settings action is Pages source = Deploy from a branch (`gh-pages`)

#### Scenario: Pages source is an owner gate

- **WHEN** code and pipelines for Pages and packages are ready
- **THEN** the task list still treats "set Pages source to Deploy from a branch (`gh-pages`)" as an owner GitHub settings action that is not performed by application code
