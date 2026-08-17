---
title: GET /v1/models
description: OpenAI-compatible model list — same union semantics as /api/tags.
sidebar:
  order: 4
---

`GET /v1/models` (and `GET /v1/models/{id}`) return the OpenAI-compatible
model list built from the **same union** as
[`GET /api/tags`](/reference/ollama/tags/): one entry per normalized model
name across healthy, non-draining nodes, with the same digest and placeholder
rules.

```bash
curl -fsS http://127.0.0.1:11435/v1/models
```
