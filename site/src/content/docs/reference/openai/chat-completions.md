---
title: POST /v1/chat/completions
description: OpenAI-compatible chat completions — SSE streaming, envelope errors, counts as client demand.
sidebar:
  order: 1
---

`POST /v1/chat/completions` — OpenAI-compatible chat passthrough over the
fleet.

```bash
curl -fsS http://127.0.0.1:11435/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b","messages":[{"role":"user","content":"hi"}]}'
```

## Contract

- Routed to nodes that **already have** the model (same holders-only contract
  as the native surface).
- `stream: false` → one OpenAI envelope (`choices[].message.content`, `usage`).
- `stream: true` → **SSE** (`data: {...}` chunks, `data: [DONE]` terminator),
  forwarded as chunks arrive.
- Counts as **client demand** for the idle timer (unlike health, capacity, or
  admin calls).
- Errors use the OpenAI envelope shape (`{"error": {...}}`) on this path.
- 503 reason codes carry `Retry-After` — see the
  [status-code reference](/faq/status-codes/).
