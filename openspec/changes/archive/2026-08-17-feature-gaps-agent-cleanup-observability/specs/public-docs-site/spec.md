## MODIFIED Requirements

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
