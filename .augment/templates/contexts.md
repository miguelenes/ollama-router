# Context Template — ollama-router

Template for context documents stored in `agents/settings/contexts/`. Adapted from the
event4u/agent-config template: Rust-flavored for this workspace.

A **context document** captures the architectural understanding of a codebase
area — its structure, key types, patterns, dependencies, and conventions. It's
a snapshot of knowledge that helps agents (and developers) quickly orient
themselves when working in that area.

## Rules for Context Documents

1. **Factual.** Based on codebase analysis, not assumptions.
2. **Navigational.** Help others find their way — point to key files and patterns.
3. **Maintainable.** Update when the codebase changes.
4. **Language:** All context documents must be written in **English**.
5. **One area per file.** Don't combine unrelated contexts.

## Context Types

| Type | When to use | Example |
|---|---|---|
| **Crate** | Document a workspace crate's structure and purpose | `ollama-router-core.md` |
| **Domain** | Document a domain across crates | `capacity-discovery.md` |
| **Service** | Document a complex service and its dependencies | `cloud-reconcile.md` |
| **Integration** | Document an external API/system integration | `verda-api.md` |
| **Infrastructure** | Document infra or DevOps concerns | `compose-stack.md` |

## Template

```markdown
# Context: {title}

> {One sentence: What area does this context cover?}

**Type:** {Crate | Domain | Service | Integration | Infrastructure}
**Created:** {YYYY-MM-DD}
**Last Updated:** {YYYY-MM-DD}
**Crate:** {crate name or "workspace-wide"}
**Related Features:** {links to feature plans or "none"}

## Overview

{2-3 sentences describing what this area does, why it exists, and who/what depends on it.}

## Key Files

| File | Purpose |
|---|---|
| `{path/to/file.rs}` | {what it does} |
| `{path/to/file.rs}` | {what it does} |

## Architecture

{How the pieces fit together. Data flow, request flow, or processing pipeline.
Use bullet points or a simple ASCII diagram.}

## Key Types & Modules

### {TypeName}

- **Location:** `{full/path.rs}`
- **Purpose:** {what it does}
- **Key methods:** `{fn1()}`, `{fn2()}`
- **Dependencies:** {what it depends on}

## Storage

| Store | Purpose | Location |
|---|---|---|
| `{file/db}` | {what it keeps} | {path, e.g. /var/lib/ollama-router/model-operations.sqlite3} |

## HTTP Surface

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/...` or `/router/v1/...` | {what it does} |

## Dependencies

### Internal
- {Other workspace crates this depends on}

### External
- {External crates, APIs, or systems}

## Patterns & Conventions

- {Patterns specific to this area}
- {Naming conventions, coding patterns}

## Known Issues / Technical Debt

- {Known problems or areas for improvement}

## Notes

{Optional: edge cases, gotchas, historical context.}
```

## Tips

- **Read `AGENTS.md` and the crate's `lib.rs`/module docs first** to gather data.
- **Link to specific files** — `crates/ollama-router/src/proxy/mod.rs` beats "the proxy".
- **Document the "why"** — why was it built this way? What tradeoffs were made?
- **Include known issues** — future agents will thank you.
- **Update `Last Updated`** whenever you modify the context.
