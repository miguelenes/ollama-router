## ADDED Requirements

### Requirement: Public promotion includes live GitHub Pages

Before or as part of making the repository public, GitHub Pages SHALL be configured to use GitHub Actions as its source, the existing Pages workflow SHALL deploy the Starlight site from the default branch, and the README SHALL link the public site URL (path base `/ollama-router/`). Flipping repository visibility from private to public remains an owner GitHub settings action gated on the existing repo-readiness files.

#### Scenario: Pages Actions source is enabled

- **WHEN** an operator completes the public-promotion checklist
- **THEN** repository Pages settings use Actions as the source and a successful `main` Pages workflow publishes the site

#### Scenario: README points at the live site

- **WHEN** a visitor opens the repository README after promotion
- **THEN** they can follow a link to the published GitHub Pages URL for this project

#### Scenario: Visibility flip is an owner gate

- **WHEN** code and workflows for Pages and packages are ready
- **THEN** the task list still treats “set repository visibility to public” as an owner gate that is not performed by application code
