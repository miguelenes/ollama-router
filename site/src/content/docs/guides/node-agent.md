---
title: Node agent
description: ollama-node-agent reports capacity and pressure from every Ollama host on :11436.
sidebar:
  order: 4
---

Every Ollama host runs `ollama-node-agent`. `setup` is elevated and
idempotent; `serve` is unprivileged and serves capacity JSON on `:11436`.

The router probes these endpoints on `:11436`:

| Path | Role |
| --- | --- |
| `GET /v1/capacity` | Memory, VRAM, GPU inventory, model presence |
| `GET /v1/pressure` | Classified RAM/VRAM pressure level |
| `GET /v1/status` | Agent + Ollama health |
| `GET /metrics` | Agent metrics (router never scrapes this for Prometheus) |

## Behavior rules

- **GiB = bytes / 1024³.** Never report GiB as decimal.
- **Soft-fail.** A down agent never makes the host unhealthy by itself — the
  router treats missing capacity data as *unknown*, which affects ranking, not
  liveness.
- **Pressure is classified on the agent.** The router trusts
  `pressure_level` and never re-classifies RAM in the router.
- **Unknown is not zero.** An omitted `vram_gb` is unknown (middle rank), not
  `0` (which would mean a measured CPU). `vram_free_gb=0` is treated as unknown
  unless `vram_free_known` says otherwise.
- GPU discovery is first-class for NVIDIA (`nvidia-smi`) and AMD ROCm
  (`rocm-smi`/`amd-smi`); Auto order is NVIDIA → macOS Metal → ROCm → CPU.
- Prometheus scrapes the **router only**, never node-agent `:11436`.
- Agent `/metrics` families use the `ollama_node_agent_*` prefix (for example
  `ollama_node_agent_ollama_up`, `ollama_node_agent_models`).

## Environment knobs

| Variable | Purpose |
| --- | --- |
| `OLLAMA_NODE_AGENT_HOST` / `OLLAMA_NODE_AGENT_PORT` | Override `serve` bind (also in config.yaml). |
| `ZROK_ENABLE_TOKEN` | zrok enable token for `setup` only (never logged). |
| `OLLAMA_NODE_AGENT_DISCOVERED_V4` | Comma-separated IPv4 list injected before UDP discovery (test seam for LAN bind). |
| `OLLAMA_NODE_AGENT_WINDOWS_ZIP` | Set to `1` on Windows to install from the standalone zip instead of `OllamaSetup.exe`. |

## macOS tunnel daemon

The `.pkg` ships the tunnel LaunchDaemon with `Disabled=true` and does not
bootstrap it on install. After `setup` reserves zrok private shares it runs
`launchctl enable` and `launchctl bootstrap` for the tunnel plist. The agent
daemon plist is unchanged and starts on pkg install.

## Enrollment (cloud hosts)

On tunneled hosts, Ollama and the agent bind loopback and a zrok **private
share** sidecar exposes them to the router. `setup`/`doctor` print a
**find this node** block with the share token **id** and enroll status — never
the raw share. The router learns the share via
`POST /router/v1/nodes/enroll` (admin bearer); enroll never writes `fleet.yaml`
and the router never SSHes into the node.

## Packaging

Release artifacts: Linux `.deb` + musl tarball, macOS `.pkg` +
LaunchDaemon, Windows MSI + LocalSystem service. The packages install the
agent only — run `sudo ollama-node-agent setup` to converge Ollama.
