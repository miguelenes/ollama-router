# request-class Specification

## Purpose
Assigns EMBED, SMALL, MEDIUM, LARGE, or GENERIC so ranking and placement can send work to the right class of machine without reading the prompt.

## Requirements

### Requirement: Classify from path, then name, then tags probe

The system SHALL classify as follows, first match wins: embed endpoints (`/api/embed`, `/api/embeddings`, `/v1/embeddings`) are EMBED; `/api/show` is GENERIC even if the model name is LARGE; `/api/pull` as an HTTP path class is Pull and MUST NOT be used as the placement size class (`model-placement` uses generate class). For generate, chat, `/v1/chat/completions`, and `/v1/completions`: embedding name markers (`embed`, `e5-`, `bge-`, `arctic-embed`) → EMBED; exact known-small bases (`moondream`, `minicpm-v`) → SMALL; a parseable `:Nb` tag suffix → SMALL / MEDIUM / LARGE using configured `small_max_b` / `medium_max_b` (MoE such as `qwen3:30b-a3b` uses total params, 30B); if `:Nb` is absent, a parseable holder-catalog `details.parameter_size` (`1B`, `1.2B`, `8.0B`) using the same thresholds; otherwise MEDIUM. Classification MUST NOT log prompts or tag bodies.

#### Scenario: :latest uses probe parameter_size

- **WHEN** a client chats `minicpm-v4.6:latest` and a healthy holder’s tags probe reported `details.parameter_size` of `1B` (or equivalent `1.xB`)
- **THEN** the request is class SMALL, not MEDIUM

#### Scenario: :Nb still wins over details

- **WHEN** a client chats `llama3.1:70b` even if a probe `parameter_size` disagrees
- **THEN** the request is class LARGE

#### Scenario: no size signal stays MEDIUM

- **WHEN** a client chats `custom-modelfile:latest` and no holder catalog has a parseable `parameter_size`
- **THEN** the request is class MEDIUM

#### Scenario: show is not LARGE

- **WHEN** a client calls `POST /api/show` for `llama3.1:70b`
- **THEN** the request is GENERIC

#### Scenario: MoE uses total params

- **WHEN** a client chats `qwen3:30b-a3b`
- **THEN** the request is class LARGE (30B total, not 3B active)

#### Scenario: OpenAI completions uses the same size class

- **WHEN** a client posts `/v1/completions` for `llama3.2:3b`
- **THEN** the request is class SMALL (same as `/api/generate` for that name)
