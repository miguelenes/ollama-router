---
name: opencode-rag
description: Semantic code & image retrieval via OpenCodeRAG — vector search, file skeletons, symbol usage lookup, and image description lookup for this workspace
---

## OpenCodeRAG Tools

This workspace has OpenCodeRAG indexed for semantic code and image search. Use these tools BEFORE planning, editing, or answering code questions.

Experiential memory via the RAG plugin is **disabled**. Persist decisions and gotchas with **mem0** (`add_memory` / `search_memories`) instead.

### Decision tree — ALWAYS follow this order

1. User mentions code behavior/architecture → `search_semantic(query)`
2. User mentions a file path → `get_file_skeleton(filePath)` THEN `read` on specific lines
3. User mentions a function/class/variable to edit → `find_usages(symbolName)` THEN `search_semantic` THEN `edit`
4. User asks a code question → `search_semantic` to gather context before answering
5. User asks about an image or visual asset → `describe_image(filePath)` (optionally pass `systemPrompt` to focus on specific features) to retrieve its generated description, then optionally `search_semantic` for related code

### When to use each tool

| Tool | Use when | Example |
|------|----------|---------|
| `search_semantic` | Any code search — find relevant code by meaning or keyword | `"authentication middleware"` |
| `get_file_skeleton` | You have a file path but need to orient before reading | `"src/plugin.ts"` |
| `find_usages` | Before editing any function, class, or variable — check all call sites | `"createRagHooks"` |
| `describe_image` | When the user refers to an image or asks "what's in this screenshot/diagram?" | `"assets/login-screen.png"` |

### Workflow

1. **Skeleton first** — call `get_file_skeleton(filePath)` to see structure
2. **Find usages** — call `find_usages(symbolName)` before modifying any symbol
3. **Search** — call `search_semantic(query)` to find relevant code
4. **Describe images** — call `describe_image(filePath)` when context involves an image file
5. **Read** — use the `read` tool on specific line ranges identified above
6. **Edit** — now you have full context to make safe changes

### Anti-patterns — NEVER do these

- Reading full files without calling `get_file_skeleton` first (wastes tokens)
- Editing a function without calling `find_usages` first (breaks call sites)
- Answering code questions without calling `search_semantic` first (you guess at behavior)
- Using `grep`/`glob` when `search_semantic` would find the answer faster
- Treating image files as text — use `describe_image` instead of reading raw bytes

### Parameters

- `search_semantic`: `query` (req), `pathHints?`, `languageHints?`, `fileExtensions?`, `topK?`
- `get_file_skeleton`: `filePath` (req)
- `find_usages`: `symbolName` (req), `pathHint?`, `topK?`
- `describe_image`: `filePath` (req), `systemPrompt?`

### Tips

- Use `pathHints` to narrow searches to specific directories
- Use `languageHints` to filter by file type
- Use `fileExtensions` to filter by file extension (e.g. `".ts"`)
- `find_usages` is essential before refactoring — it shows every reference
- If no results appear, the workspace may not be indexed yet — run `opencode-rag index`
