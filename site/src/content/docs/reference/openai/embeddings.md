---
title: POST /v1/embeddings
description: OpenAI-compatible embeddings — NOT rewritten to /api/embed; counts as client demand.
sidebar:
  order: 3
---

`POST /v1/embeddings` — OpenAI-compatible embeddings passthrough.

```bash
curl -fsS http://127.0.0.1:11435/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"qwen3-embedding:8b","input":"hello"}'
```

- Response follows the OpenAI shape: `data[].embedding`, `usage`.
- This path is **not rewritten** to `/api/embed` (the rewrite applies to
  native `POST /api/embeddings` only).
- EMBED-class ranking applies (lower known VRAM preferred); holders-only.
- Counts as client demand for the idle timer.
- Errors use the OpenAI envelope shape; 503 codes carry `Retry-After` — see
  the [status-code reference](/faq/status-codes/).
