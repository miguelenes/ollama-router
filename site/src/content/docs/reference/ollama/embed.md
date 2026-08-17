---
title: POST /api/embed
description: Ollama embeddings over the fleet — EMBED class prefers nodes with lower known VRAM.
sidebar:
  order: 3
---

`POST /api/embed` — generate embeddings over the fleet.

```bash
curl -fsS http://127.0.0.1:11435/api/embed \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3-embedding:8b","input":"hello"}'
```

## Routing

Embedding requests use the **EMBED** class: among eligible holders, nodes
with **lower known `vram_gb`** rank first (unknown VRAM ranks after known, and
a measured CPU can win an equal-load tie). Embeddings still only go to nodes
that already have the model.

## Response

`{ "embeddings": [[...]] }` for a string input or one vector per input item.
Errors follow the [status-code reference](/faq/status-codes/); embedding
bodies are never logged.
