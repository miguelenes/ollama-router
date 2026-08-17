## Why

The router is now positioned as a brand-new application, so legacy compatibility paths add complexity and testing burden without delivering product value. Removing backward-compatibility behaviors now keeps the API and configuration surface smaller, clearer, and cheaper to maintain.

## What Changes

- Remove legacy compatibility behavior across HTTP APIs, config/env aliases, and historical behavior shims that were kept only for older clients or migration windows.
- Standardize on the current product contract only (Ollama-native and OpenAI-compatible surfaces documented in active specs), returning explicit non-support responses for removed legacy paths.
- Remove legacy feature flags and compatibility toggles so behavior is deterministic by default.
- Remove known persisted legacy metadata and shims, including FleetState legacy `extra` keys such as `thunder_instance_id` and `tailscale_ip`, plus compatibility status parsing paths kept only for older binaries.
- Remove node-agent setup cleanup paths that exist solely for retired install mechanisms (for example, legacy Windows scheduled-task cleanup).
- Replace historical "capacity-agent" terminology in maintained code comments and operator docs with current "node-agent" terminology where applicable.
- **BREAKING**: Requests relying on deprecated routes, payload formats, response shims, or legacy configuration aliases will no longer be accepted.

## Capabilities

### New Capabilities
- `legacy-compatibility`: Defines required removal of legacy compatibility surfaces and the fail-fast behavior clients must receive when using retired contracts.

### Modified Capabilities
- None. All normative requirements for this change are captured in `legacy-compatibility`.

## Impact

- Affected code: request parsing/validation and compatibility branches in `crates/ollama-router/src/http/`, `crates/ollama-router/src/proxy/`, and routing/placement logic in `crates/ollama-router-core/src/`.
- Additional affected code: FleetState backward-compat metadata handling in `crates/ollama-router-core/src/fleet/state.rs`, status compatibility shims in `crates/ollama-router-core/src/jobs/`, and legacy setup cleanup in `crates/ollama-node-agent/src/setup/`.
- Affected tests: endpoint contract tests, compatibility-path regression tests (to be removed/replaced), and routing/placement behavior tests.
- Operational impact: deployment and client docs must clearly communicate removed compatibility inputs and migration expectations.
- Security/sensitivity: no change to secret handling policy; removed branches must not introduce extra logging of request bodies during validation failures.
