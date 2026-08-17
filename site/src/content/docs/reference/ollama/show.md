---
title: POST /api/show
description: Model information from the holder only — 503 model_missing when no node holds it.
sidebar:
  order: 7
---

`POST /api/show` returns model details (`modelfile`, `parameters`, `template`,
…) from the node that **holds the model** — never a ranked guess across the
fleet.

```bash
curl -fsS http://127.0.0.1:11435/api/show \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b"}'
```

When no healthy node holds the model, the router returns
[503 `model_missing`](/faq/status-codes/) instead of falling back to another
node's answer.
