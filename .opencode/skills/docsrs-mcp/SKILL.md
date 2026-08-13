---
name: docsrs-mcp
description: Looks up rustdoc JSON from docs.rs via the docsrs MCP (lookup_crate_items, lookup_item, search_crate, lookup_impl_block). Use when coding against Rust crates, needing type signatures, fields, trait impls, or crate structure; when the user mentions crates.io, docs.rs, rustdoc, Axum, Tokio, reqwest, tower-http, or serde; and when Context7 is quota-limited or thin on rustdoc.
---

# docs.rs rustdoc (docsrs MCP)

Do not invent crate APIs from training data. Fetch rustdoc.

MCP server: **docsrs** (Cursor `user-docsrs`). Binary: `/home/menes/.cargo/bin/docsrs-mcp`. No auth, no secrets.

## When

| Need | Tool |
|------|------|
| Guides, examples, migration notes | **Context7** (`resolve-library-id` → `query-docs`) |
| Signatures, fields, methods, trait impls | **docsrs** |
| Context7 quota / empty rustdoc | **docsrs** |
| Types already in this workspace | rust-analyzer / read source — not MCP |

## Tools

Omit `version` unless the user pins one. The server reads `Cargo.lock` in the working directory, else latest on docs.rs.

1. **Explore** — `lookup_crate_items` with `crate_name` (e.g. `axum`). Optional `module_path` (`axum::extract`).
2. **Detail** — `lookup_item` with `crate_name` + `item_path` (`routing::Router`, `sync::Mutex`).
3. **Find** — `search_crate` with `crate_name` + `query`. Optional `limit` (default 20).
4. **Impls** — `lookup_impl_block` with `crate_name` + `item_path`.

Call `GetMcpTools` for this server before invoking if the schema is not already loaded.

## This workspace (ollama-router)

Stack lock: Axum **0.8**, Tokio, reqwest `rustls-tls`, tower-http, tracing, serde (ignore unknown fields), thiserror. No Axum 0.7 GraphQL/WS guides.

Context7 IDs when using guides:

- Axum 0.8: `/tokio-rs/axum` (`axum_v0_8_4`) or `/websites/rs_axum`
- Tokio: `/tokio-rs/tokio` or `/websites/rs_tokio`
- reqwest: `/seanmonstar/reqwest` or `/websites/rs_reqwest`
- tower-http: `/websites/rs_tower-http`

## Do not

- Log or persist request/response bodies, tokens, or prompts.
- Follow Axum 0.7 GraphQL/WebSocket recipes.
- Treat docsrs-mcp's own openssl/native-tls deps as allowed in this crate (`deny.toml` still bans them).
