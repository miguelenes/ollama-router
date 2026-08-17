# Roadmap: Agents-dir structural cleanup

> Bring the freshly scaffolded `agents/` tree to the package-canonical layout by moving the contexts dir to `agents/settings/contexts/`.

## Prerequisites

- [x] Read `AGENTS.md` (agent-config layer notes) and `.augment/templates/contexts.md`

## Context

Produced by `/optimize agents-dir --audit` (post-scaffold re-run). The
`--scaffold` step-2 shorthand created `agents/contexts/`, but the canonical
context path is `agents/settings/contexts/` per the audit inventory, the mode
table, the package contexts template, and `layered-settings.md`
(`agents/settings/.agent-settings.yml` alongside `contexts/`, `policies/`).
One decision resolves both the dir and the template wording: **move** (chosen
below — package-canonical) or ratify `agents/contexts/` as this repo's own
convention (see Notes).

## Phase 1: Structural cleanup

- [x] Move `agents/contexts/` → `agents/settings/contexts/` (`mkdir -p agents/settings && mv agents/contexts agents/settings/contexts`), preserving `.gitkeep`
- [x] Update `.augment/templates/contexts.md`: storage path `agents/contexts/` → `agents/settings/contexts/`

## Acceptance Criteria

- [x] `agents/settings/contexts/.gitkeep` exists
- [x] `agents/contexts/` no longer exists
- [x] `.augment/templates/contexts.md` contains `agents/settings/contexts/` and no longer contains the bare `agents/contexts/` path
- [x] `find agents -type d` lists only `agents`, `agents/roadmaps`, `agents/features`, `agents/settings`, `agents/settings/contexts`

## Notes

Alternative if the user prefers this repo's own convention: keep
`agents/contexts/`, update `.augment/templates/contexts.md` to match, and
close this roadmap as skipped. Default action recorded here is the move
(package-canonical); `/optimize agents-dir --fix` asks per-action
confirmation, so the alternative can still be chosen there.
