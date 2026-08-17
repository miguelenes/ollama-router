---
title: POST /api/embeddings
description: Compatibility alias — rewritten to /api/embed for Ollama ≤0.32 clients.
sidebar:
  order: 4
---

`POST /api/embeddings` is **rewritten to `/api/embed`** by the router for
compatibility with Ollama ≤0.32 clients. Semantics are identical to
[`POST /api/embed`](/reference/ollama/embed/): same routing, same EMBED class
preference, same response shape.

The OpenAI path `POST /v1/embeddings` is **not** rewritten — see the
[OpenAI reference](/reference/openai/embeddings/).
