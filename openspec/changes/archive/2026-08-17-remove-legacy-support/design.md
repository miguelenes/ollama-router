## Context

The change removes legacy compatibility behavior while preserving the current fleet-router contract described in active specs. See `proposal.md` for motivation and scope framing. Implementation must avoid accidental behavior expansion, especially in request parsing, endpoint routing, and configuration loading where compatibility shims tend to accumulate.

## Goals / Non-Goals

**Goals:**
- Remove legacy-only API routes, payload coercions, and config/env aliases from router startup and request handling.
- Keep current Ollama-native and OpenAI-compatible interfaces unchanged for supported clients.
- Make unsupported legacy usage fail fast with deterministic, machine-readable errors and safe observability.
- Keep behavior aligned with existing invariants around routing, placement, and sensitivity.

**Non-Goals:**
- Reworking routing algorithms, class preference, or placement strategy beyond removing compatibility branches.
- Introducing new backward-compatible aliases or migration toggles.
- Changing node-agent core runtime/serve semantics beyond removing legacy install cleanup paths, or changing cloud provider semantics or fleet source-of-truth model.
- Removing current production features that are not legacy support (for example `sticky_affinity`, `capacity_url`, and current zrok-based enroll fields).

## Decisions

### 1) Introduce a single compatibility-removal sweep by surface area
- **Decision:** Implement removals across explicit surfaces: HTTP endpoints/payload parsing, configuration/env loading, compatibility feature toggles, FleetState legacy metadata, jobs status forward-compat parsing, and node-agent legacy install cleanup.
- **Rationale:** This reduces the risk of partial cleanup that leaves hidden legacy codepaths.
- **Alternatives considered:**
  - **Incremental endpoint-by-endpoint cleanup**: rejected because legacy behavior can remain reachable through config aliases or shared parser utilities.
  - **Runtime deprecation layer**: rejected because the request explicitly requires no old support.

### 2) Fail closed for unsupported legacy inputs
- **Decision:** Legacy paths and payloads return explicit non-success responses rather than automatic rewrites.
- **Rationale:** Rejecting unsupported contracts is clearer for operators and prevents silent behavior divergence.
- **Alternatives considered:**
  - **Auto-translate legacy payloads**: rejected as continued support.
  - **Silently ignore unsupported fields**: rejected because it obscures client errors and complicates debugging.

### 3) Remove compatibility aliases from startup configuration
- **Decision:** Only canonical config keys/env vars are accepted; deprecated aliases are treated as invalid and fail-fast at startup.
- **Rationale:** Startup is the safest place to enforce contract changes with clear operator feedback.
- **Alternatives considered:**
  - **Alias with warning logs**: rejected as legacy support.
  - **Hidden alias mapping**: rejected because it masks drift and increases maintenance cost.

### 4) Keep migration messaging deterministic and non-sensitive
- **Decision:** Standardize error identifiers and concise migration hints without logging request bodies/prompts.
- **Rationale:** Preserves observability and sensitivity requirements while still helping clients migrate.
- **Alternatives considered:**
  - **Verbose response/body echoing for debugging**: rejected due to sensitivity policy.

### 5) Strip known legacy FleetState metadata instead of round-tripping it
- **Decision:** Explicitly remove known retired keys such as `thunder_instance_id` and `tailscale_ip` from FleetState load/persist paths.
- **Rationale:** Preserving these keys in `extra` silently perpetuates legacy contract state and weakens migration boundaries.
- **Alternatives considered:**
  - **Keep round-trip in `extra` forever**: rejected because it is effectively indefinite compatibility support.
  - **Ignore on read but preserve on write**: rejected because stale keys remain and can reappear in tooling expectations.

## Known legacy surfaces to remove

- FleetState compatibility metadata round-trip in `crates/ollama-router-core/src/fleet/state.rs` (`extra` keys including `thunder_instance_id` and `tailscale_ip`).
- Legacy status forward-compat parsing branch in `crates/ollama-router-core/src/jobs/types.rs`.
- Node-agent Windows legacy scheduled-task cleanup path in `crates/ollama-node-agent/src/setup/windows.rs`.
- Remaining maintained comments/docs that use historical "capacity-agent" terminology where "node-agent" is now canonical.

## Risks / Trade-offs

- **[Risk] Hidden downstream clients still use retired contracts** → **Mitigation:** Add explicit integration tests for removed paths and document migration hints in release notes.
- **[Risk] Over-removal could break currently supported API contracts** → **Mitigation:** Keep contract tests for supported Ollama-native and OpenAI-compatible endpoints green throughout the change.
- **[Risk] Legacy logic is shared with active paths and removal causes regressions** → **Mitigation:** Isolate compatibility branches before deletion and add focused unit tests around parse/validation boundaries.

## Migration Plan

1. Inventory all legacy compatibility entry points (routes, parsers, config aliases, toggles) and map each to a canonical replacement or explicit unsupported response.
2. Remove or disable each legacy path in code, keeping only current canonical handling.
3. Add/adjust tests for:
   - supported canonical contracts still succeeding;
   - retired contracts failing deterministically;
   - no sensitivity regressions in logs/metrics.
4. Update operator-facing docs/changelog with removed legacy behaviors and migration guidance.
5. Rollout in one release with clear BREAKING label; rollback is reverting this change set if unexpected client impact appears.

## Open Questions

None.
