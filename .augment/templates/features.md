# Feature Plan Template — ollama-router

Template for feature plan files stored in `agents/features/`. Adapted from
the event4u/agent-config template: quality gates and references are
Rust-flavored for this workspace (see `AGENTS.md`).

## Rules for Feature Plans

1. **Collaborative.** Feature plans are created interactively with the user, not auto-generated.
2. **Decision-focused.** Capture problems, proposals, scope, and tradeoffs — not implementation steps.
3. **Linked.** Reference affected crates, related features, and generated roadmaps.
4. **Language:** All feature plans must be written in **English**.
5. **One feature per file.** Don't combine unrelated features.
6. **Keep it concise.** Aim for 100–300 lines. If larger, the feature should be split.

## Status Values

| Emoji | Status | Meaning |
|---|---|---|
| 💡 | Idea | Rough concept, not yet validated |
| 🔍 | Exploring | Being researched and brainstormed |
| 📋 | Planned | Structured plan complete, ready for roadmap |
| 🗺️ | Roadmapped | Roadmap(s) generated, ready for implementation |
| 🔄 | In Progress | Implementation started |
| ✅ | Complete | Feature shipped |
| ❌ | Rejected | Decided not to build |
| ⏸️ | On Hold | Paused for external reasons |

## Template

```markdown
# Feature: {title}

> {One sentence: What does this feature do and why?}

**Status:** 💡 Idea
**Created:** {YYYY-MM-DD}
**Author:** {name}
**Issue:** {GitHub issue link or "none"}
**Crate:** {crate name or "workspace-wide"}
**Context:** {path to context document or "none"}

## Problem

{What pain point does this solve? Who is affected? What happens today without this feature?}

## Proposal

{What's the proposed solution? Keep it high-level — describe the outcome, not the implementation.}

## Scope

### In Scope

- {What this feature includes}
- {Specific functionality}

### Out of Scope (deferred)

- {What this feature does NOT include}
- {Features to consider later}

## Affected Areas

| Area | Impact |
|---|---|
| Crate: {name} | {what changes} |
| Config: {fleet.yaml section / knob} | {new or changed tunables} |
| HTTP: {/api/... or /router/v1/...} | {new/changed routes} |
| Metrics: {ollama_router_*} | {new/changed gauges} |
| Compose: {deploy/...} | {infra changes} |

## Technical Approach

{High-level architecture decisions. Which patterns to follow? Which existing modules to extend?
Reference existing code where helpful — e.g. `crates/ollama-router-core/src/routing/`.}

### Options Considered

| Option | Pros | Cons | Decision |
|---|---|---|---|
| {Option A} | {pros} | {cons} | ✅ Chosen / ❌ Rejected |
| {Option B} | {pros} | {cons} | ✅ Chosen / ❌ Rejected |

## Open Questions

- [ ] {Unresolved question 1}
- [ ] {Unresolved question 2}

## Dependencies

- {Other features or changes this depends on}
- {External crates or services needed}

## Acceptance Criteria

- [ ] {Measurable outcome 1}
- [ ] {Measurable outcome 2}
- [ ] `task check` passes
- [ ] `task coverage` ≥ 80% lines

## Roadmaps

_No roadmaps generated yet. Create one under `agents/roadmaps/` from the roadmap template._

## Notes

{Optional: edge cases, risks, references, related discussions.}
```

## Tips

- **Start with the Problem.** If you can't articulate the problem, the feature isn't ready.
- **Be specific in Scope.** "Out of Scope" is as important as "In Scope".
- **List Affected Areas early.** This helps estimate effort and identify risks.
- **Use Options Considered.** Document why you chose one approach over another.
- **Link to code.** "See `crates/ollama-router-core/src/config/knobs.rs`" is better than "the knobs module".
