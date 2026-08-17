---
title: GET /api/version
description: Router-owned version, not a ranked Ollama version.
sidebar:
  order: 8
---

`GET /api/version` returns the **router's own version** — the fleet proxy
version (`0.1.0` preview), not a version picked from whichever Ollama node
ranks first.

```bash
curl -fsS http://127.0.0.1:11435/api/version
# {"version":"0.1.0"}
```
