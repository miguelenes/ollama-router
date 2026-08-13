---
title: ollama-router wiki
tags: [index, ollama-router]
sourceRefs:
  - AGENTS.md
lastReviewed: 2026-08-13
---

# ollama-router wiki

Standalone Rust fleet proxy. Verda Cloud spots only — no Thunder, no RunPod.

- [[concepts/ollama-router-product]] — Env-first fleet, Tailscale-only URLs, Verda spots, no YAML inventory.
- [[concepts/ollama-router-idle-scale-down]] — Router-owned idle teardown; `inflight_inc` is the only activity signal.
- [[concepts/ollama-router-load-share]] — Utilization WLC + class preference.
- [[concepts/ollama-router-phase-3-retry-and-memory-safety]] — Pre-first-byte retry, bounded NDJSON, warm-keeper inflight.
- [[concepts/ollama-router-durable-model-operations]] — SQLite pull/delete recovery via live `/api/tags`.
- [[concepts/ollama-router-audit-remediation]] — Five production remediations to preserve.
- [[concepts/ollama-cloud-vram-guardrails]] — Inclusive 8–80 GiB Verda GPU window.
- [[concepts/ollama-capacity-discovery]] — Sibling Rust agent on `:11436`; GiB = 1024³; soft-fail.
