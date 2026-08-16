# deploy

ollama-router

Stack: Compose / infra

Existing MCP coverage already fits this Rust networking/observability project (docs.rs, Grafana, Prometheus, Docker, Context7, GitHub), so no new MCP servers need persisting. Recommend two extra skills (rust-router and ollama), two short project rules, adding rs/toml to RAG include extensions, and excluding target from RAG indexing.

## Commands

Docker Compose / Swarm orchestration. Nested apps (for example `console/` or `nas-agent/`) have their own stacks. Never require Laravel Boost at this root.

- `docker compose config --quiet` — validate compose
- `docker compose ps` — service status

## Agent environment

- Open **this folder** as the workspace in Cursor or OpenCode.
- User-global Cursor (`~/.cursor/mcp.json`) and OpenCode MCP already provide MemoryAI, Context7, and GitHub.
- Project `.cursor/mcp.json` / `opencode.json` `mcp` is stack-specific only.
- Cursor rules: `.cursor/rules/*.mdc`
- OpenCode: `opencode.json` loads `AGENTS.md` and Cursor rules
- OpenCodeRAG is OpenCode-only (see block below)
