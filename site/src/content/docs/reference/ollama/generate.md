---
title: POST /api/generate
description: Ollama text generation over the fleet — NDJSON streaming, holders-only routing.
sidebar:
  order: 2
---

`POST /api/generate` — single-prompt generation over the fleet.

```bash
curl -fsS http://127.0.0.1:11435/api/generate \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b","prompt":"hi","stream":false}'
```

## Routing

Same holders-only contract as `/api/chat`: only nodes that already have the
model are eligible. A miss is 503 `model_missing`, never a silent pull.

## Streaming

- `stream: false` → one JSON object (`response`, timings, done).
- `stream: true` → NDJSON lines, one token per line, `done: true` at the end.

## Errors

Ollama-style error strings; 503 reason codes with `Retry-After` are
[documented here](/faq/status-codes/). Prompts are never logged.
