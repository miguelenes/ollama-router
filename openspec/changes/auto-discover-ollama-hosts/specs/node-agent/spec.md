## ADDED Requirements

### Requirement: LAN heartbeat can enroll without zrok shares

When `register.url` (or setup `--enroll-url`) is set, the agent SHALL periodically POST `POST /router/v1/nodes/enroll` with the fail-closed admin bearer. If private share ids are present, the existing share-token payload SHALL be used (including the configured `register.origin`). If share ids are absent, the agent SHALL send `origin: discovered` unless `register.origin` is explicitly `adopt`, plus node id (configured or hostname), `hostname`, and direct `ollama_url` / `capacity_url` built from a discovered private IPv4 or Tailscale CGNAT address plus the local Ollama and agent ports. The LAN no-share path MUST NOT send `origin` `fleet`, `verda`, or `runpod`. The heartbeat MUST NOT run when neither share ids nor a usable private/CGNAT address exist. The heartbeat MUST NOT log share tokens, enable tokens, the admin bearer, or Ollama request bodies.

#### Scenario: LAN agent enrolls with direct URLs

- **WHEN** `register.url` is set, no zrok shares are reserved, `register.origin` is unset or not `adopt`, and the host has private IPv4 `192.168.100.160` with Ollama on `:11435` and the agent on `:11436`
- **THEN** the heartbeat POSTs those HTTP URLs with `origin: discovered` and does not send share ids

#### Scenario: LAN agent may enroll as adopt when configured

- **WHEN** `register.url` is set, no zrok shares are reserved, and `register.origin` is `adopt`
- **THEN** the heartbeat POSTs LAN URLs with `origin: adopt` and does not send `fleet`, `verda`, or `runpod`

#### Scenario: No address and no shares skips the beat

- **WHEN** `register.url` is set but the host has neither share ids nor a private/CGNAT IPv4
- **THEN** the agent does not POST enroll and emits a reason-coded skip without secrets

#### Scenario: Share heartbeat still wins on tunneled hosts

- **WHEN** `register.url` is set and private share ids are reserved
- **THEN** the heartbeat sends share ids as today and does not replace them with a public or CGNAT Ollama URL
