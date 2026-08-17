## MODIFIED Requirements

### Requirement: Site is published to GitHub Pages

The site SHALL be built and deployed to GitHub Pages by the Woodpecker Pages pipeline on the repository's default branch, with GitHub Pages configured to serve the `gh-pages` branch. Publishing SHALL NOT require a manual upload or a checkout of the live site.

#### Scenario: Push to default branch publishes the site

- **WHEN** a change lands on the default branch and the Pages pipeline runs successfully
- **THEN** the site is built, `site/dist` is published to the `gh-pages` branch, and the public Pages URL serves the updated content

#### Scenario: Failed build does not replace the live site

- **WHEN** the Pages pipeline build or deploy step fails
- **THEN** the previously published site remains reachable and no partial artifact is served

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
