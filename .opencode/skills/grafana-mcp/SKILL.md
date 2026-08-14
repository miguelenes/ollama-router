---
name: grafana-mcp
description: Uses the Grafana MCP (mcp-grafana) against local compose Grafana at http://127.0.0.1:3000 for dashboards, Prom/Loki queries via Grafana, deeplinks, and incidents. Use when the user mentions Grafana, dashboards, panels, Loki logs in Grafana, OnCall, or fleet overview UI; when editing provisioned dashboard JSON under deploy/observability/grafana/; and when presenting live observability with clickable links.
---

# Grafana MCP

MCP server: **grafana** (Cursor `user-grafana`). Started via `uvx mcp-grafana` with `GRAFANA_URL=http://127.0.0.1:3000`. Requires `task compose:up` (or equivalent) so Grafana is reachable.

Prefer this MCP for **dashboards / UI / Loki-through-Grafana**. For raw PromQL against the scrape TSDB, prefer skill `prometheus-mcp`.

## When

| Need | Tool path |
|------|-----------|
| Find a dashboard | `search_dashboards` → then summary |
| Understand a dashboard | `get_dashboard_summary` (never dump full JSON first) |
| One panel / field | `get_dashboard_property` with JSONPath |
| Edit a dashboard | `patch_dashboard` (targeted); avoid full `update_dashboard` |
| Live Prom via Grafana | `query_prometheus` with a **bounded time range** |
| Logs | `query_loki_logs` with label matchers (not bare `{job=~".+"}`) |
| Share a view | `generate_deeplink` |
| Writes (create/update alerts, incidents, datasources) | Only if the user explicitly asks |

Call `GetMcpTools` for this server before invoking if schemas are not loaded.

## This workspace

Compose home dashboard is **Ollama Router** (`uid: ollama-router`). Do not replace it with the nodes dashboard.

| UID | Title | JSON |
|-----|-------|------|
| `ollama-router` | Ollama Router (home) | `deploy/observability/grafana/dashboards/ollama-router.json` |
| `ollama-router-nodes` | Nodes | `.../ollama-router-nodes.json` |
| `ollama-router-jobs` | Model operations | `.../ollama-router-jobs.json` |
| `ollama-router-logs` | Logs | `.../ollama-router-logs.json` |
| `ollama-router-verda` | Verda | `.../ollama-router-verda.json` |
| `compose-scrapes` etc. | Stack mixins | `.../dashboards/stack/` |

Datasource UIDs in panels: `prometheus`, `loki`. Provisioning: `deploy/observability/grafana/provisioning/`.

Repo JSON under `deploy/observability/grafana/dashboards/` is GitOps source of truth — after MCP edits, sync or regenerate those files when the change should ship.

## Do not

- Pull full dashboard JSON with `get_dashboard_by_uid` when a summary/property suffices.
- Scrape or chart node-agent `:11436` (router scrape only).
- Log prompts, bodies, Verda tokens, zrok share tokens, or admin bearer (sensitivity allowlist only).
- Invent panel queries — read existing panels or live metrics first.
