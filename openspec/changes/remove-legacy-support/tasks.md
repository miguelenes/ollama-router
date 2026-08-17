## 1. Legacy Surface Inventory

- [x] 1.1 Enumerate all legacy compatibility routes, payload coercions, config/env aliases, and compatibility toggles in router crates.
- [x] 1.2 Map each legacy surface to either a canonical supported replacement or an explicit unsupported-response behavior.
- [x] 1.3 Produce an inventory table (surface, file, keep/remove, migration note) to drive implementation and review.

## 2. Remove Legacy API Compatibility Paths

- [x] 2.1 Remove retired HTTP routes and payload translation branches from request handlers and proxy entry points.
- [x] 2.2 Ensure legacy endpoint/payload calls fail with deterministic machine-readable errors and migration hints.
- [x] 2.3 Verify canonical Ollama-native and OpenAI-compatible request contracts continue to work unchanged.
- [x] 2.4 Strip known legacy FleetState compatibility keys (`thunder_instance_id`, `tailscale_ip`) from load/persist behavior and remove round-trip tests expecting their preservation.
- [x] 2.5 Remove retired jobs forward-compat parsing shims that exist only for legacy status encodings.

## 3. Remove Legacy Configuration Compatibility

- [x] 3.1 Delete deprecated config/env alias handling in config parsing and validation paths.
- [x] 3.2 Enforce fail-fast startup validation for unsupported legacy config keys and deprecated environment aliases.
- [x] 3.3 Remove dead compatibility toggles and update defaults to current-only behavior.
- [x] 3.4 Remove node-agent setup cleanup paths that exist only for retired install mechanisms (including legacy Windows scheduled-task cleanup).

## 4. Tests and Observability Safety

- [x] 4.1 Add/update unit and integration tests for rejected legacy routes and payload forms.
- [x] 4.2 Add/update tests for rejected legacy config aliases and canonical config acceptance.
- [x] 4.3 Confirm unsupported legacy usage observability records allowlisted metadata only (no request bodies, prompts, or secrets).
- [x] 4.4 Add/update tests for FleetState legacy key stripping on load and persist (`thunder_instance_id`, `tailscale_ip`).
- [x] 4.5 Add/update tests for jobs status parsing: legacy-only values fail deterministically and current contract values still parse.

## 5. Documentation and Verification

- [x] 5.1 Update docs/changelog with BREAKING migration guidance for all removed legacy surfaces.
- [x] 5.2 Replace maintained "capacity-agent" wording with "node-agent" where terminology is historical rather than product-canonical.
- [x] 5.3 Run `task check` and fix any formatting, lint, test, or deny failures.
- [x] 5.4 Run `task coverage` and keep workspace line coverage at or above 80%.
