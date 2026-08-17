---
title: Quick start
description: Run ollama-router with Docker or the native dev flow in minutes.
sidebar:
  order: 1
---

Install [Task](https://taskfile.dev/), then build and run the router container:

```bash
task docker
docker run --rm -p 11434:11434 ollama-router:local
curl -fsS http://127.0.0.1:11434/healthz
# {"status":"ok","version":"0.1.0"}
```

`task docker` builds the default `router` target of the single root
`Dockerfile` (`docker buildx bake router` is equivalent). The mock and
node-agent images are targets of the same file.

## Native dev flow

With host Ollama already on `:11434`, the router runs on `:11435` and the
node agent on `:11436` (the dev flow does **not** run `setup`):

```bash
task dev
curl -fsS http://127.0.0.1:11435/healthz
curl -fsS http://127.0.0.1:11435/api/tags
curl -fsS http://127.0.0.1:11435/metrics | grep ollama_router_inflight
```

Send your first request through the router:

```bash
curl -fsS http://127.0.0.1:11435/api/generate \
  -H 'Content-Type: application/json' \
  -d '{"model":"llama3.2:3b","prompt":"hi","stream":false}'
```

`auto_pull_on_miss` stays **false**, so a miss never pulls Hub models onto the
real disk. A model that no node holds returns
[503 `model_missing`](/faq/status-codes/), not a pull.

## Observability

```bash
task compose:up   # Grafana :3000 / Prometheus :9090 on loopback
task obs:open     # dashboard URLs
```

Prometheus scrapes the **router only** (`:11435`) — never the node-agent
`:11436`. Grafana home is the fleet overview dashboard.

## Without host Ollama

`task compose:mock` builds a canned CPU+GPU mock fleet and a router container
on host `:11435`. Do not run it at the same time as `task compose:up` (same
Grafana/Prometheus ports). The mock stack enables `auto_pull_on_miss` so a miss
enqueues a placement-aware fleet pull (503 `pull_enqueued` + `Retry-After`);
the committed default stays **false**.
