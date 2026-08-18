# Self-hosted zrok (OpenZiti + zrok)

Cloud and remote Ollama hosts reach the router through a **private zrok share**,
not Tailscale and not `zrok.io` public shares. The control plane runs **next to
the router**. `fleet.yaml` LAN URLs stay **direct HTTP**.

This repo does not vendor the full OpenZiti stack. `task zrok:fetch` sparse-clones
**zrok v1.1.11** `docker/compose/zrok-instance` (CLI: `enable`, `reserve private`,
`share reserved`, `access private`). Do not use `https://get.openziti.io/zrok-instance/fetch.bash`
(404) or fetch.bash from `main` (that is **zrok2**, incompatible with this
product). Remove `.local/zrok` and re-fetch if a zrok2 compose landed there.

Prometheus in `deploy/compose.yaml` scrapes the **router** only. Do not scrape
node-agent `:11436`.

## Local compose

```bash
task zrok:fetch   # official compose → .local/zrok (once)
task zrok:up      # writes .local/zrok/.env if missing (gitignored secrets)
task zrok:ps
```

`task zrok:up` copies [`deploy/zrok.env.example`](../zrok.env.example) to
`.local/zrok/.env` and fills empty passwords with local random values. That
file is gitignored. Controller API on the host: **http://127.0.0.1:18080**.
Cloud guests need a public URL (Tailscale Funnel **8443** → `:18080`, e.g.
`https://desktop.bicorn-beta.ts.net:8443`). OpenZiti **3022** must stay
direct TLS (Funnel TCP **10000**) — do not HTTP-proxy the data plane. Set
advertised ziti addresses **before** the first `zrok:up`; changing them later
does not rewrite PKI.

Create an enable token from the controller (command names follow upstream):

```bash
docker compose -f deploy/compose.zrok.yaml exec zrok-controller \
  zrok admin create account dev@localhost 'your-password'
```

Save the printed enable token in the **environment**, never in git:

```bash
export ZROK_ENABLE_TOKEN='…'          # enable token from create account
export ZROK_API_ENDPOINT=http://127.0.0.1:18080
```

CLI smoke test against that API (host network so the container sees loopback):

```bash
task zrok:cli
# equivalent:
docker run --rm --network host \
  -e ZROK_API_ENDPOINT=http://127.0.0.1:18080 \
  docker.io/openziti/zrok:1.1.11 version
```

Point the router overlay at the instance (`deny_unknown_fields` tunables YAML):

```yaml
tunnel:
  zrok_bin: zrok
  api_endpoint: http://127.0.0.1:18080
  enable_token_env: ZROK_ENABLE_TOKEN
  access_bind: "127.0.0.1"
```

Env knobs: `OLLAMA_ROUTER_ZROK_API_ENDPOINT`,
`OLLAMA_ROUTER_ZROK_ENABLE_TOKEN_ENV`, `OLLAMA_ROUTER_ZROK_ACCESS_BIND`.
The enable **token** is read from the env named by `enable_token_env` (default
`ZROK_ENABLE_TOKEN`). Never put the token in YAML.

`task compose:up` (Grafana `:3000` / Prometheus `:9090`) does **not** start
zrok. Stop with `task zrok:down`.

## Production

1. Run the same official zrok-instance compose (or Linux install) on the
   router host. Use TLS in production (Caddy overlay in upstream docs).
2. Wildcard DNS is required for **public** shares. This product uses **private**
   shares only; still run a controller/frontend the `zrok` CLI can reach.
3. Set `tunnel.api_endpoint` to a **guest-reachable** controller (Funnel
   `https://desktop.bicorn-beta.ts.net:8443`, not `127.0.0.1` and not MagicDNS
   without Funnel). Private shares do not need wildcard DNS; the ziti
   controller/router advertised addresses still must be reachable from RunPod.
4. `tunnel.access_bind` stays loopback (`127.0.0.1`). Enroll hydrates
   `http://127.0.0.1:<port>` into FleetState.
5. Give the router process `ZROK_ENABLE_TOKEN` (or the env named in
   `enable_token_env`) so it can `zrok enable` once. Share tokens are **not**
   enable tokens.
6. Remote Ollama and node-agent bind **loopback**. The private share is the
   only ingress.

## Agent (enable command, not a VPN)

Install the packaged agent, then enable against **your** controller:

```bash
export ZROK_API_ENDPOINT=http://127.0.0.1:18080   # or https://zrok.example
export ZROK_ENABLE_TOKEN='…'                      # enable token, not a share token
sudo --preserve-env=ZROK_API_ENDPOINT,ZROK_ENABLE_TOKEN \
  ollama-node-agent setup
```

`setup`/`doctor` print a **find this node** block (share token **id** + enroll
status). Enroll that share with `POST /router/v1/nodes/enroll` (admin bearer).
It does not write `fleet.yaml`. Same-LAN hosts keep direct URLs in `fleet.yaml`
and do not need a tunnel.

Optional agent YAML (`deny_unknown_fields`):

```yaml
tunnel:
  enable: true
  zrok_bin: zrok
  api_endpoint: http://127.0.0.1:18080
```

Do not install Tailscale. Do not put enable or share tokens in config.yaml.
