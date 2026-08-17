---
title: GET /api/tags
description: CLI-compatible union of on-disk models across healthy nodes.
sidebar:
  order: 5
---

`GET /api/tags` returns a fleet-wide model catalog as a
**CLI-compatible union**: one row per normalized model name across healthy,
non-draining nodes, so the official Ollama CLI `list` command treats the
router as one Ollama.

```bash
curl -fsS http://127.0.0.1:11435/api/tags
```

## Row rules

- `digest` is always a string of **at least 12 characters** (the CLI slices
  `digest[:12]`). When a node's probe omitted the digest, the router emits a
  placeholder — a SHA-256 of the model name.
- When the probe supplied `size`, `modified_at`, native `details`, or
  `capabilities`, the aggregated row includes them.
- `details.router_nodes` lists the node ids that hold the model.
- Duplicate names collapse to one row (newest `modified_at`, then
  lexicographically smallest node id).

`GET /v1/models` and `GET /v1/models/{id}` use the same union — see the
[OpenAI reference](/reference/openai/models/).
