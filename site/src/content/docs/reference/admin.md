---
title: Admin API
description: /router/v1/* OpenAPI reference — bearer auth, fail-closed.
sidebar:
  order: 5
---

The admin API lives under `/router/v1/*` and covers live inventory, model
ensure/delete, jobs, drain/undrain, reload, readiness, and the Verda/RunPod
managers.

## Authentication

Every admin endpoint requires `Authorization: Bearer $OLLAMA_ROUTER_ADMIN_TOKEN`.
There is **no default secret**: when `OLLAMA_ROUTER_ADMIN_TOKEN` is unset, the
entire admin API returns **403** (fail-closed).

```bash
curl -fsS http://127.0.0.1:11435/router/v1/nodes \
  -H "Authorization: Bearer ${OLLAMA_ROUTER_ADMIN_TOKEN}"
```

## OpenAPI reference

The machine-readable OpenAPI document lives in the repository at
`site/openapi/openapi.yaml`. A rendered version ships with the site:

<iframe src="/ollama-router/openapi.html" title="Admin API OpenAPI reference" style="width:100%;min-height:70vh;border:1px solid var(--sl-color-hairline);border-radius:8px"></iframe>

[Open the reference full-screen](/ollama-router/openapi.html) if the embed is
too small.
