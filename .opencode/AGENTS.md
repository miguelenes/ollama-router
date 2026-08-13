# Finish check

After any Rust, Cargo, Taskfile, or Dockerfile change, do **not** claim the task done until the local gate is green.

1. Clear rust-analyzer / IDE diagnostics on edited files.
2. Run **`task check`** (sequential `fmt --check`, clippy `-D warnings`, `cargo test --workspace --locked`, `cargo deny`).
3. If fmt `--check` fails, run `cargo fmt --all`, then re-run `task check`.
4. Fix clippy, test, deny, and analyzer failures. Do not leave warnings.

Skip the cargo gate only for docs / rules / plan-only work with **no** code or lockfile changes. Keep using Task — do not add a Makefile, justfile, or npm scripts.

```bash
# BAD — stop after edits
cargo check

# GOOD — gate matches CI intent
task check
```

# OpenCodeRAG index config

Keep `opencode-rag.json` in sync with the tree. When a change alters **what is
indexed**, **how it is chunked**, or **how it is described**, update that file
in the same change. `openCode.autoIndex` already watches; do not run
`opencode-rag index --force` unless asked.

- New source extension → `indexing.includeExtensions`; add `chunking.nodeTypes`
  for that language if tree-sitter should split functions/types
- New generated, cache, or vendor directory → `indexing.excludeDirs`
- New lockfile, log, or secret pattern → `indexing.excludeFiles`
- Retrieval wording no longer matches the stack → `embedding.queryPrefix`
  and/or `description.systemPrompt`

Walker uses `path.extname()`. Bare `excludeDirs` names match **any path
segment**; entries with `/` match **prefixes**. Never exclude `.opencode`
wholesale — wiki and skills live there; keep `.opencode/rag_db`,
`.opencode/node_modules`, and `.opencode/plugins`.

Leave `"mcp": { "enabled": false }` in `opencode-rag.json` unless an external
MCP client must connect to the RAG plugin. Project MCP servers (docsrs,
grafana, prometheus) live in `opencode.json` under `"mcp"` — see skills
`docsrs-mcp`, `grafana-mcp`, `prometheus-mcp` and `.cursor/rules/project-mcps.mdc`.
Do not register the OpenCodeRAG plugin via an OpenCode `"plugin"` config key
(use `.opencode/plugins/*.js` auto-discovery).

```text
# BAD — excludeDirs: [".opencode"]
# GOOD — excludeDirs: [".opencode/rag_db", ".opencode/node_modules", ".opencode/plugins"]
```
