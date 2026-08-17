# legacy-compatibility Specification

## Purpose

Define and enforce a clean, current-only router contract by removing legacy compatibility behavior so clients either use supported interfaces or receive explicit, deterministic failures.

## Requirements

### Requirement: Router accepts only current API contracts

The router MUST reject legacy endpoint paths, deprecated request payload forms, and compatibility-only coercions that are not part of current supported Ollama-native and OpenAI-compatible interfaces.

#### Scenario: Legacy endpoint is called

- **WHEN** a client sends a request to a retired legacy route
- **THEN** the router returns a non-success HTTP status and a machine-readable error indicating the route is unsupported

#### Scenario: Deprecated payload shape is sent

- **WHEN** a client sends a request body requiring legacy field translation or compatibility coercion
- **THEN** the router rejects the request instead of rewriting it to a current shape

### Requirement: Legacy config aliases are not honored

The router MUST fail fast on deprecated compatibility config keys and environment aliases at startup, and MUST only apply actively supported configuration names.

#### Scenario: Deprecated config key is present

- **WHEN** startup configuration contains a legacy compatibility key
- **THEN** startup fails fast with a clear validation error naming the unsupported key

#### Scenario: Deprecated environment alias is set

- **WHEN** a deprecated environment alias is provided for behavior that has a current canonical variable
- **THEN** startup fails fast with a clear validation error naming the unsupported alias

### Requirement: Legacy FleetState metadata is stripped

The router MUST strip known legacy compatibility metadata keys from FleetState entries during load and persist flows, and MUST NOT keep them in round-tripped `extra` metadata.

#### Scenario: Legacy FleetState key exists on load

- **WHEN** a FleetState entry contains a known retired key such as `thunder_instance_id` or `tailscale_ip`
- **THEN** the key is removed from in-memory state and is not preserved on subsequent writes

#### Scenario: FleetState is rewritten after migration

- **WHEN** FleetState is persisted after any state mutation
- **THEN** known retired compatibility keys are absent from the resulting file

### Requirement: Legacy forward-compat parsing shims are removed

The router MUST remove compatibility parsing branches that exist only to interpret retired state/status encodings from older binaries when those encodings are no longer part of the supported contract.

#### Scenario: Retired compatibility status value is encountered

- **WHEN** a status value only supported by a legacy forward-compat shim is provided
- **THEN** parsing fails deterministically instead of silently mapping it to a current status

#### Scenario: Current status values are provided

- **WHEN** status values from the current contract are provided
- **THEN** parsing succeeds with no behavior change

### Requirement: Removed legacy surfaces produce explicit migration guidance

For each removed legacy interface, the router MUST provide a deterministic error code/message shape that points operators to the supported replacement contract.

#### Scenario: Client depends on retired compatibility behavior

- **WHEN** a request matches a removed compatibility behavior
- **THEN** the response includes an error identifier and concise migration hint to a supported route or schema

#### Scenario: Observability for unsupported usage

- **WHEN** the router rejects a legacy compatibility path
- **THEN** metrics and logs record only allowlisted metadata and never include request bodies, prompts, or secrets
