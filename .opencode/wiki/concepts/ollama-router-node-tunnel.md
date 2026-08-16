---
title: Node tunnel (self-hosted zrok private share)
tags: [ollama-router, zrok, openziti, enroll, verda, runpod]
sourceRefs:
  - crates/ollama-node-agent
  - crates/ollama-router-core/src/fleet
  - crates/ollama-router/src/http/admin.rs
  - crates/ollama-router/src/tunnel.rs
  - crates/ollama-router-verda
  - crates/ollama-router-runpod
lastReviewed: 2026-08-16
---

# Node tunnel

Hosts that are **not** on the same L2/L3 as the router are reached through a
**self-hosted zrok private share** (Apache 2.0), not SSH and not Tailscale.
The zrok/OpenZiti control plane runs **next to the router**. `zrok.io` is not
a required dependency. Public zrok shares are forbidden.

This page is the contract. The **node-agent** installs Ollama and a zrok sidecar
(`ollama-node-agent-tunnel`). The router hydrates reachability via
`POST /router/v1/nodes/enroll`. The router never SSH. Enroll does **not**
write `fleet.yaml`.

## Private share only

- `ollama-node-agent setup` already installs Ollama. It also starts a zrok
  sidecar by **spawning the zrok binary**. Do not vendor a Go SDK. Do not add
  a TUN/VPN.
- On tunneled hosts, Ollama (`:11434`) and the node-agent (`:11436`) bind
  **loopback**. The private share is the only ingress.
- `fleet.yaml` LAN URLs stay **direct HTTP**. Same-LAN hosts do not need a
  tunnel.

## Find it / enroll

`setup` and `doctor` print one **find this node** block: redacted share token
**id** (prefix, never the full unique-name) plus enroll status
(`pending` / `configured`). That is the installer "find it" artifact. The
router learns the share via **enroll**, not SSH.

`POST /router/v1/nodes/enroll` is fail-closed admin bearer
(`OLLAMA_ROUTER_ADMIN_TOKEN`, unset → 403). Allowlisted JSON only: node id or
proposed id, origin (`fleet` | `verda` | `runpod` | `adopt`), zrok share
unique-names for Ollama and the capacity agent, agent version, hostname. No
Ollama request bodies. No zrok enable tokens (`deny_unknown_fields`).

- **origin=fleet** — existing `fleet.yaml` / Permanent registry row only.
  Hydrate reachability. Never write `fleet.yaml`.
- **origin=verda** — update the existing FleetState row the Verda manager
  already owns (`managed_by=verda`). Do not create a second node. Unknown
  instance → `404` `unknown_verda_node`. Wrong owner → `409` `verda_not_owned`.
- **origin=runpod** — same pattern for `managed_by=runpod` rows. Unknown →
  `404`; wrong owner → `409` (RunPod mirror of the Verda conflict). Never invent
  a RunPod node via enroll.
- **origin=adopt** — same rules as `PUT /router/v1/nodes` (debug adopt; must
  not write `fleet.yaml`).

On success the router starts or reuses a **zrok access** frontend bound to
`tunnel.access_bind` (default `127.0.0.1:<port>`, loopback only). FleetState
persists `url=http://127.0.0.1:PORT`, `capacity_url=http://127.0.0.1:PORT`,
share unique-names (not enable keys), and `tunnel_backend=zrok`. Health
continues to `GET /api/tags` on `url`.

Router tunables (`deny_unknown_fields`):

- `tunnel.api_endpoint` — self-hosted controller (`ZROK_API_ENDPOINT`). Empty
  uses process env / zrok config. Env overlay: `OLLAMA_ROUTER_ZROK_API_ENDPOINT`.
- `tunnel.enable_token_env` — **name** of the env holding the enable token
  (default `ZROK_ENABLE_TOKEN`). Never a literal. Overlay:
  `OLLAMA_ROUTER_ZROK_ENABLE_TOKEN_ENV`.
- `tunnel.access_bind` — loopback host for `--bindAddress` (default
  `127.0.0.1`). Overlay: `OLLAMA_ROUTER_ZROK_ACCESS_BIND`.

Local compose: `task zrok:up` fetches official OpenZiti + zrok compose into
`.local/zrok` (too large to vendor). Tokens stay in `.local/zrok/.env`. See
`deploy/zrok/README.md`. Prometheus still scrapes the router only — never
node-agent `:11436`.

Agent enable (not a VPN):

```bash
export ZROK_API_ENDPOINT=http://127.0.0.1:18080
export ZROK_ENABLE_TOKEN='…'
sudo --preserve-env=ZROK_API_ENDPOINT,ZROK_ENABLE_TOKEN ollama-node-agent setup
```

Production inventory is still `fleet.yaml` + FleetState + Verda/RunPod managers.
Enroll hydrates reachability only.

The agent heartbeat (`register.url` or setup `--enroll-url`) POSTs that
allowlisted body. `--enroll-token-env` stores the env *name* in `state.json`;
the bearer is never written into `config.yaml`.

## `public_url_blocked`

Applies to globally routable IPv4 and IPv6 (including IPv4-mapped /
IPv4-compatible v6 such as `[::ffff:8.8.8.8]`), CGNAT (`100.64/10`), and
unspecified addresses (`0.0.0.0`, `::`). Hostname public tunnels
(`*.zrok.io` plus `tunnel.public_share_suffixes`) are the same reason code.
Loopback, RFC1918, link-local, and unique-local (`fc00::/7`) stay probeable.
No public-proxy fallback. RunPod public pod IPs and RunPod proxy hostnames are
also `public_url_blocked`.

## Verda

Bootstrap is a Verda **startup script**, not SSH. The router ensures a catalog
script exists (`GET`/`POST /v1/scripts`, default name `ollama-router-agent-init`)
and attaches `startup_script_id` on instance create. The script:

- downloads the matching `ollama-node-agent` `.deb` or tarball (or
  `verda.agent_package_url`)
- installs it and runs `ollama-node-agent setup` (elevated)
- writes enroll URL + zrok enable / admin tokens and `ZROK_API_ENDPOINT` from a
  0600 env file (values injected from router env + `tunnel.api_endpoint` at
  create; never committed; never echoed)
- does not install Tailscale and does not curl public `:11434`

The Verda create payload has `startup_script_id` only (no inline script field).
DTOs ignore extra JSON fields.

The router may still upload an SSH public key to satisfy Verda's API; it must
**never SSH** and must not wait on public SSH. After the instance is `running`,
the manager polls FleetState for enroll of `verda-{instance_id}`, then probes
the tunnel `GET /api/tags`. Timeout → allowlisted `enroll_timeout`, keep
ownership, do not persist provider errors in SQLite. The guest heartbeat may
send the create hostname as `id`; enroll maps that onto the existing Verda
FleetState row. Enroll then updates that row only. Preferred images stay
Ubuntu 24 CUDA.

## RunPod

Bootstrap is container **`dockerStartCmd`**, not SSH and not a Verda-style
startup-script catalog. The manager renders the agent-bootstrap script
(install node-agent package, zrok private enable, agent serve, enroll) and
passes `dockerStartCmd: ["bash","-lc", <script>]` on a configurable stock
CUDA-capable `image`. Container `env` carries secret values under configured
`*_env` names only — memory-only, never logged or persisted. Create uses
`ports: []` and `volumeInGb: 0` so the RunPod proxy/public IP is never ingress
and terminated pods leave no billed volume.

After the pod is `RUNNING`, the manager polls FleetState for enroll of the
managed RunPod row, then probes tunnel `GET /api/tags`. Public pod IP / RunPod
proxy hostnames stay `public_url_blocked`. Teardown always **terminates**
(`DELETE /pods/{id}`), never stop-only. Interrupted (non-RUNNING) managed pods
are terminated on the reconcile tick and replaced only when below the floor.

## Related

- [[concepts/ollama-router-product]]
- [[concepts/ollama-capacity-discovery]]
