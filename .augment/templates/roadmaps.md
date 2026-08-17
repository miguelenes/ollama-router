# Roadmap Template — ollama-router

Template for roadmap files stored in `agents/roadmaps/`. Adapted from the
event4u/agent-config template: condensed to the rules that are self-contained
here (no maintainer lint/council tooling), quality gates are this repo's
`task` pipeline.

## Rules for Roadmaps

1. **State the goal first.** One sentence at the top — what is the outcome?
2. **Checkboxes are mandatory, not decorative.** Every active roadmap MUST
   contain at least one `- [ ]` per phase. Glyph semantics:
   `[ ]` open · `[x]` done · `[~]` deferred (follow-up roadmap) · `[-]` cancelled.
3. **Status is `ready` (default) or `draft`.** Drafts declare frontmatter
   `status: draft` and are hidden until flipped. No other status values.
4. **List prerequisites** — what must exist or be running before starting.
5. **Reference existing code** — point to files, crates, or modules.
6. **Acceptance criteria must be agent-decidable** — a command exit code, a
   file that exists, a test that passes. Never "user approves" or "looks good".
7. **Quality gates** — `task check` (fmt --check, clippy -D warnings,
   test --locked, deny) and `task coverage` (≥ 80% lines, `**/main.rs` ignored).
8. **Language:** All roadmap files must be written in **English**.
9. **One task per file.** Don't combine unrelated work.
10. **No tags, releases, or version numbers.** Roadmaps describe work, not
    shipping. Never write "Target release: X.Y.Z" or phase version suffixes.
11. **Merge is never a completion requirement.** A roadmap is complete when
    its checkboxes are ticked and verification ran; commit/merge/push stays
    the user's call.
12. **Lifecycle:** every roadmap ends in one folder:
    `agents/roadmaps/` (active), `agents/roadmaps/archive/` (work happened),
    `agents/roadmaps/skipped/` (decision against), `agents/roadmaps/later/`
    (blocked-for-later, `status: later` + a `Blocked until` line).
13. **Size:** keep ≤ 600 lines; split by phase if larger.

## Quality Gates

Run locally with Task (see `AGENTS.md`; CI runs the cargo commands directly):

```bash
task check     # fmt --check → clippy -D warnings → test --locked → cargo deny
task coverage  # cargo llvm-cov --fail-under-lines 80, ignore **/main.rs
```

Do not author whole-pipeline steps as checkboxes when the remote CI is the
authoritative gate — prefer narrow verifications (a targeted test filter, a
`cargo check -p <crate>`, a grep).

## Template

```markdown
---

# Roadmap: {Short descriptive title}

> {One sentence: What is the expected outcome?}

## Prerequisites

- [ ] Read `AGENTS.md` and relevant crate docs
- [ ] {specific prerequisites}

## Context

{Why this roadmap exists. Which crate/domain. Links to issues and feature plans.}

- **Feature:** {path to feature plan or "none"}
- **Issue:** {GitHub issue link or "none"}

## Phase 1: {Phase name}

- [ ] **Step 1:** {Clear, actionable instruction}
- [ ] **Step 2:** {Next step — reference files/crates}
- [ ] ...

## Phase 2: {Phase name}

- [ ] **Step 1:** {description}
- [ ] ...

## Acceptance Criteria

- [ ] {Observable, testable criterion}
- [ ] `task check` passes
- [ ] `task coverage` ≥ 80% lines

## Notes

{Optional: edge cases, decisions, links to related docs.}
```

## Tips

- **Don't describe architecture** the agent can read from `AGENTS.md` — just reference it.
- **Do reference specific files:** "See `crates/ollama-router/src/proxy/mod.rs`"
  is better than "look at the proxy."
- **Do define boundaries:** State what the agent should NOT touch or change.
- **Do split large tasks** — an agent works better with a focused 500-line file
  than a sprawling 2000-line one.
- **One task per file.** Don't combine unrelated work.
