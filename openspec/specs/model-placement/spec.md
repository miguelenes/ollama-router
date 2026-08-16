# model-placement Specification

## Purpose
Puts a pulled model on every node that can actually run it so later generate/chat/embed can load-share across holders instead of pinning one disk.

## Requirements

### Requirement: Default pull targets every generate-class-eligible node

Default `POST /api/pull` and admin ensure (placement selector `*` / omitted) SHALL target every **healthy** node that passes label policy and **static** VRAM for that model’s **generate** class (`request-class` on `/api/generate`, not the `/api/pull` path class). The job MUST NOT target a node that fails those static gates. LARGE MUST NOT target a known CPU. MEDIUM MUST NOT target a known CPU (static VRAM below the medium minimum). MEDIUM and LARGE MUST NOT target unknown VRAM (`size-load-routing`). Explicit `#all` MAY include additional nodes; capacity skips at run still apply. Success SHALL mean the model is on disk on the remaining targets (visible on the next tags probe). The system MUST NOT log pull bodies.

#### Scenario: MEDIUM pull includes the known GPU, not the CPU

- **WHEN** the fleet is a known CPU (VRAM 0, gpus 0) plus a known 24 GiB GPU, and the client pulls `qwen3:8b` (MEDIUM) with default placement
- **THEN** the GPU is a target if it meets the medium VRAM minimum; the known CPU is not a target

#### Scenario: LARGE pull stays off CPU and off unknown VRAM

- **WHEN** the fleet is a known CPU, a known GPU that meets the LARGE estimate, and a third node with unknown VRAM, and the client pulls `llama3.1:70b`
- **THEN** the CPU and the unknown-VRAM node are not targets; the known GPU is

#### Scenario: HTTP pull path is not the placement class

- **WHEN** a client calls `POST /api/pull` for `llama3.1:70b`
- **THEN** placement uses LARGE generate-class gates, not an always-admit Pull path class
