## Purpose

Publishes the ollama-router container image to GitHub Container Registry so public consumers can pull a tagged package from the repository's packages surface.

## MODIFIED Requirements

### Requirement: Router image is published to GHCR

On a successful publish pipeline run for the default branch or a version tag, the system SHALL push the bake target `router` to `ghcr.io/<owner>/<repo>` (lowercase) with metadata tags covering the default branch edge tag, git SHA, and semver patterns when a version tag is present.

#### Scenario: Push to main publishes an edge package

- **WHEN** a change lands on the default branch and the GHCR publish pipeline runs successfully
- **THEN** a package exists at `ghcr.io/<owner>/<repo>` tagged for that commit (edge and/or sha) and is listed under the repository's GitHub Packages

#### Scenario: Version tag publishes semver and latest

- **WHEN** a `v*` tag is pushed and the publish pipeline succeeds
- **THEN** the image is tagged with the matching semver patterns and `latest`

### Requirement: Provenance attestation when the repository is public

When the GitHub repository is public, the publish pipeline SHALL attach build provenance attestation for the pushed router image digest. When the repository is private, attestation SHALL be skipped (GitHub does not support attestations on user-owned private repos).

#### Scenario: Public repo attests the pushed digest

- **WHEN** the repository is public and a router image digest is pushed
- **THEN** an attestation for that subject digest is recorded and pushed to the registry

#### Scenario: Private repo skips attestation

- **WHEN** the repository is still private
- **THEN** the publish pipeline completes without failing on attestation and does not require attestation APIs
