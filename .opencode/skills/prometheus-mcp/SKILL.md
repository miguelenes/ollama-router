---
name: prometheus-mcp
description: Queries the local Prometheus MCP (prometheus-mcp-server) at http://127.0.0.1:9090 for PromQL instant/range queries, metric discovery, metadata, and scrape targets. Use when debugging metrics, validating scrapes, writing or checking PromQL, investigating up/targets, or when the user mentions Prometheus, PromQL, /metrics, or ollama_router_* series. Prefer over Grafana MCP for raw TSDB work; use prometheus-configuration for writing scrape YAML.
---

# Prometheus MCP

MCP server: **prometheus** (Cursor `user-prometheus`). Started via `uvx prometheus-mcp-server` with `PROMETHEUS_URL=http://127.0.0.1:9090`. Requires compose Prometheus (or another scrape on that URL).

Prefer this MCP for **raw PromQL / targets / metric inventory**. For dashboards, Loki, and deeplinks, use skill `grafana-mcp`. For authoring `prometheus.yml` / rules, use skill `prometheus-configuration`.

## When

| Need | Tool |
|------|------|
| Is Prometheus up? | `health_check` |
| What is scraped? | `get_targets` (`state`: active/dropped/any; optional `scrape_pool`) |
| Find series names | `list_metrics` with `filter_pattern` (e.g. `ollama_router`) |
| Type / help text | `get_metric_metadata` |
| Point-in-time value | `execute_query` |
| Graph / trend | `execute_range_query` with `start`, `end`, `step` (keep ranges tight) |

Call `GetMcpTools` for this server before invoking if schemas are not loaded.

## This workspace

Scrape config: `deploy/observability/prometheus.yml`.

- Job `ollama-router` scrapes host router `:11435` `/metrics` via `host.docker.internal`.
- **Do not** add a scrape for node-agent `:11436`.
- Hot metrics live in the router binary (`crates/ollama-router/src/http/metrics.rs`); never label by model name.
- `vram_free_gb=0` means **unknown**, not empty GPU.
- `node_info` labels stay `node`, `origin`, `role` only.

Useful prefixes / series:

- `ollama_router_requests_total`, `ollama_router_request_duration_seconds`, `ollama_router_inflight`
- `ollama_router_node_healthy`, `ollama_router_node_vram_*`, `ollama_router_node_pressure`, `ollama_router_node_info`
- `ollama_router_route_reason_total`, `ollama_router_verda_*`, `ollama_router_job_*`
- `ollama_router_aggregated_models`, `ollama_router_node_models`, `ollama_router_discovery_total`

Example checks:

```text
up{job="ollama-router"}
sum by (node) (ollama_router_inflight)
sum by (reason) (rate(ollama_router_route_reason_total[5m]))
```

## Do not

- Broad unbounded range queries (always set start/end/step).
- Expect model-name labels on hot metrics.
- Log request bodies, prompts, or secrets from correlated debugging.
- Treat this skill as YAML authoring — that is `prometheus-configuration`.
