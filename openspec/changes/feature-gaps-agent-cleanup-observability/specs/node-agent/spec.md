## Purpose

Hardens the node-agent installer, supervisor, and packaging so a package install never leaves a broken or crash-looping service, packaging artifacts are verified against the binary that generates them, setup failures surface instead of being silently ignored, and exposed metrics use one consistent prefix.

## ADDED Requirements

### Requirement: Package install never leaves a crash-looping tunnel service

A macOS `.pkg` install SHALL NOT leave an enabled tunnel LaunchDaemon that crashes on hosts where no share tokens have been reserved. Tunnel daemons shipped in the package SHALL be installed disabled (launchd disabled, or otherwise not auto-starting), and the tunnel service SHALL only become active after `setup` reserves shares. The agent serve daemon itself MAY start on install.

#### Scenario: Fresh pkg install does not crash-loop

- **WHEN** the macOS pkg is installed on a host that has never run `setup` (no reserved share tokens)
- **THEN** no tunnel daemon is running or crash-looping, and the agent daemon is unaffected

#### Scenario: Setup activates the tunnel

- **WHEN** `setup` completes and has reserved a zrok private share
- **THEN** the tunnel daemon becomes active and supervised

### Requirement: Packaging verifies generated artifacts against the binary

The Linux deb/tarball and macOS pkg build scripts SHALL fail when the checked-in unit or plist text drifts from what the installed binary generates. The macOS pkg check SHALL compare the binary-generated plist text against the checked-in plist (the same pattern as the Linux unit check), not a file against itself.

#### Scenario: Drifted plist fails the package build

- **WHEN** the checked-in tunnel plist is edited without updating the plist text the binary generates
- **THEN** `pack-pkg.sh` exits non-zero and no package is produced

#### Scenario: Matching artifacts package cleanly

- **WHEN** the checked-in units and plists match the binary-generated text
- **THEN** the packaging scripts succeed

### Requirement: Setup surfaces write failures

`setup` SHALL fail with a non-zero exit and a visible error when it cannot write a file it is responsible for, including the Ollama environment file on macOS. Silent best-effort ignores of failed writes in the converge path MUST NOT remain.

#### Scenario: Env-file write failure fails setup

- **WHEN** `setup` on macOS cannot write the Ollama environment file (for example, unwritable directory)
- **THEN** setup exits non-zero with an error naming the path, instead of continuing as if it succeeded

### Requirement: Convergence probes the Ollama version once

macOS and Windows `setup` converge SHALL probe the installed Ollama version at most once per run; duplicated subprocess spawns for the same value MUST NOT remain.

#### Scenario: Single version probe per converge

- **WHEN** `setup` converges on macOS or Windows with Ollama installed
- **THEN** the Ollama version is probed exactly once during that converge run

### Requirement: Agent metrics use one consistent prefix

Every series the agent exposes on `GET /metrics` SHALL use the `ollama_node_agent_` prefix. Bare `ollama_up`, `ollama_models`, `ollama_gpu_vram_gb`, `ram_available_gb`, and `gpu_utilization_pct` series MUST NOT remain. Metrics MUST NOT carry model-name labels.

#### Scenario: Agent metric families are prefixed

- **WHEN** the agent's `/metrics` endpoint is scraped
- **THEN** every metric family name starts with `ollama_node_agent_` and no bare `ollama_up` or `ollama_models` family exists

### Requirement: Agent environment knobs are documented

Every environment variable the agent reads SHALL be documented in the shipped config comments or the node-agent docs. Undocumented debug-only knobs SHALL either be documented or removed.

#### Scenario: No undocumented knobs

- **WHEN** the agent's source is searched for environment-variable reads
- **THEN** each one (including `OLLAMA_NODE_AGENT_DISCOVERED_V4` and `OLLAMA_NODE_AGENT_WINDOWS_ZIP`) appears in config comments or the node-agent documentation, or the knob no longer exists
