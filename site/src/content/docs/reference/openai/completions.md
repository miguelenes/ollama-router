---
title: POST /v1/completions
description: OpenAI-compatible legacy completions — SSE streaming, counts as client demand.
sidebar:
  order: 2
---

`POST /v1/completions` — OpenAI-compatible legacy completions passthrough.

Same contract as
[`/v1/chat/completions`](/reference/openai/chat-completions/): holders-only
routing, `prompt` instead of `messages`, SSE streaming when `stream: true`,
OpenAI envelope errors, and it counts as client demand for the idle timer.

```bash
curl -fsS http://127.0.0.1:11435/v1/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b","prompt":"hi"}'
```
