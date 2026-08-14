# Security Policy

ollama-router is an Ollama-compatible fleet proxy. It forwards generate, chat, and embed traffic and may hold Verda, zrok, and admin credentials. Please report security issues privately so we can fix them before public disclosure.

## Supported Versions

Security updates apply only to the **latest production release** (semver tags) and to fixes landing on `main`. Older releases and development branches do not receive security backports unless we explicitly announce otherwise.

| Version | Supported |
| ------- | ------------------ |
| Latest production release | :white_check_mark: |
| Older releases | :x: |
| `main` / unreleased development | Security fixes land here first; not a support commitment for deployments |

## Reporting a Vulnerability

**Do not** open a public GitHub issue, pull request, or discussion for a security vulnerability.

### Preferred: GitHub private advisory

Use GitHub’s private vulnerability reporting for this repository:

https://github.com/miguelenes/ollama-router/security/advisories/new

(If that link is unavailable, enable *Private vulnerability reporting* under the repository’s Security settings.)

Please **do not** include prompts, request/response bodies, embeddings, `/api/chat` messages, Verda access/refresh tokens or client secrets, zrok share or enable tokens, SSH private keys, `OLLAMA_ROUTER_ADMIN_TOKEN`, `OLLAMA_NODE_AGENT_TOKEN`, or other capacity bearer tokens. Redact or use synthetic fixtures.

### What to expect

| Stage | Target |
| ----- | ------ |
| Initial acknowledgment | Within **3 business days** |
| Status update | At least every **7 days** until resolved or declined |
| Fix / advisory (if accepted) | As soon as practical; severity and exploitability drive priority |

**If accepted:** we will coordinate a fix, credit you if you want acknowledgment, and disclose only after a fix is available (or after we agree a disclosure timeline).

**If declined:** we will explain why (e.g. not reproducible, out of scope, accepted risk, or already fixed).

### Scope (in)

- Remote code execution, injection, or privilege escalation in the proxy or routing path
- Bypass of admin `/router/v1/*` bearer auth (`OLLAMA_ROUTER_ADMIN_TOKEN`)
- Bypass of node-agent `/v1/*` bearer auth, or serving `/v1/*` on a public `:11436` without a token
- Exposure of Verda, zrok, admin, or capacity credentials
- Treating a public `:11434` URL as healthy (`public_url_blocked` bypass)
- Server-side request forgery via node or upstream URLs

### Scope (out)

- Denial of service from volume alone, without a clear application bug
- Issues that require physical access, a compromised operator workstation, or stolen valid credentials with no further escalation
- Vulnerabilities solely in third-party dependencies already tracked by Dependabot (unless you have a concrete exploit path in this proxy)
- Social engineering of operators or end users

Thank you for helping keep ollama-router and the traffic it proxies safe.

## Node agent (`ollama-node-agent`)

The agent on each Ollama host listens on **`:11436`**.

- `GET /healthz` and `GET /metrics` are unauthenticated (no request bodies).
- `GET /v1/*` requires `Authorization: Bearer` when `token` / `OLLAMA_NODE_AGENT_TOKEN` is set.
- `listen: all` / `0.0.0.0` **refuses to start** if the bearer token is empty. Do not expose `:11436` on a public interface without a token.
- Linux/Verda hosts typically bind **loopback**; a self-hosted zrok private share is the ingress. `listen: lan` is optional for same-LAN hosts.
- On tunneled remote hosts, **Ollama stays on loopback** (`:11434`). Do not publish it on a public interface.
- Privileged `setup` is CLI-only (no HTTP install/pull APIs). zrok enable tokens (`ZROK_ENABLE_TOKEN`) and share tokens are secrets: setup-only / enroll-only, never committed, never written into the `serve` unit.
- The long-running `serve` process must not hold `VERDA_*` or `ZROK_ENABLE_TOKEN`. The `tunnel` sidecar spawns the zrok binary and must not log enable or share tokens.
