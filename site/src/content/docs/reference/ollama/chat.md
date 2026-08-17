---
title: POST /api/chat
description: Ollama chat completions with messages, routed to nodes that already hold the model.
sidebar:
  order: 1
---

`POST /api/chat` — chat completion over the fleet.

```bash
curl -fsS http://127.0.0.1:11435/api/chat \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b","messages":[{"role":"user","content":"hi"}],"stream":false}'
```

## Routing

The request targets a healthy node that **already has** the model (class
SMALL / MEDIUM / LARGE by size). A model no node holds returns
[503 `model_missing`](/faq/status-codes/) — the router does not pull on miss
(`auto_pull_on_miss` is false by default).

## Streaming

- `stream: false` → one JSON object.
- `stream: true` → **NDJSON**: one JSON object per line, forwarded as it
  arrives. `done: true` on the final line. The router never buffers the whole
  stream, and once a stream has begun it never retries another node.

## Errors

Ollama-style error strings on this path. The 503 reason codes are
[documented here](/faq/status-codes/), always with a `Retry-After` header.
Chat messages and prompts are never logged by the router.
