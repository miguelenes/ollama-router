---
title: ollama-router wiki
tags: [index, ollama-router]
sourceRefs:
  - AGENTS.md
lastReviewed: 2026-08-16
---

# ollama-router wiki

Standalone Rust fleet proxy. Optional Verda spots and RunPod interruptible
pods — Thunder stays forbidden.

- [[concepts/ollama-router-product]] — Env-first fleet, tunnel/loopback-only cloud URLs, Verda+RunPod, no YAML inventory.
- [[concepts/ollama-router-node-tunnel]] — Self-hosted zrok private share, enroll token, loopback bind, Verda startup script / RunPod dockerStartCmd.
- [[concepts/ollama-router-idle-scale-down]] — Router-owned idle teardown; `inflight_inc` is the only activity signal.
- [[concepts/ollama-router-load-share]] — Utilization WLC + class preference.
- [[concepts/ollama-router-phase-3-retry-and-memory-safety]] — Pre-first-byte retry, bounded NDJSON, warm-keeper inflight.
- [[concepts/ollama-router-durable-model-operations]] — SQLite pull/delete recovery via live `/api/tags`.
- [[concepts/ollama-router-audit-remediation]] — Five production remediations to preserve.
- [[concepts/ollama-cloud-vram-guardrails]] — Inclusive 8–80 GiB cloud GPU window.
- [[concepts/ollama-capacity-discovery]] — Sibling Rust agent on `:11436`; GiB = 1024³; soft-fail.
