## Purpose

Gives operators complete and honest visibility into the router: every request the router serves or rejects is counted and reason-coded, in-flight work is visible as gauges, dashboards reference only series production actually scrapes, and readiness reflects whether the fleet can serve anything at all.

## ADDED Requirements

### Requirement: Every request path records RED metrics

Every HTTP response the router produces on `/api/*` and `/v1/*` — including body-cap 400/413 rejections, unknown-path 404s, 501 non-fleet-operation responses, pull/delete/unload branches, and OpenAI model-by-id 404s — SHALL increment a requests counter labeled with request class and status code, and every rejection SHALL record a route reason on the reason counter. These counters MUST NOT carry model-name labels.

#### Scenario: Rejections are counted

- **WHEN** a client receives 501 for `POST /api/create` and 404 for an unknown OpenAI path in the same scrape window
- **THEN** the requests counter shows increments with `code="501"` and `code="404"`, and both rejections appear on the route-reason counter

#### Scenario: Pull branches are counted

- **WHEN** `POST /api/pull` returns 400 for a missing model and later a successful pull returns 200
- **THEN** both responses increment the requests counter with their class and status, and the 400 records a route reason

### Requirement: Rejections and job branches are logged with reason codes

Local rejections (4xx, 501) and pull/delete/unload failure branches SHALL emit a tracing log line carrying the path, request class, and status or reason code, plus the model name when the request carries one. The log MUST NOT contain bodies, prompts, embeddings, or any secret material.

#### Scenario: Unknown path rejection reaches Loki

- **WHEN** a client requests an unknown `/v1/*` path
- **THEN** a log line with the path and the reason code (for example `unknown_path`) is emitted with the router request fields

### Requirement: In-flight jobs and pool capacity are gauges

The system SHALL export a gauge of currently running model operations (pull/delete) by kind, returning to zero when every running job reaches a terminal state, and a gauge of available upstream connection permits so admission-wait pressure is visible.

#### Scenario: A running pull is visible in Prometheus

- **WHEN** a pull job is running and is later canceled
- **THEN** the running-jobs gauge shows the job while running and drops when the job reaches a terminal state

### Requirement: Dashboards reference only emitted series

The production nodes dashboard SHALL use router-emitted series — `ollama_router_node_ollama_up` and `ollama_router_node_models` — and MUST NOT reference node-agent-only series such as bare `ollama_up` or `ollama_models`. Mock-compose dashboards MAY reference agent series, only under their `ollama_node_agent_*` names.

#### Scenario: Production nodes panels resolve to router series

- **WHEN** Prometheus scrapes the router in production (no `:11436` job) and the nodes dashboard is opened
- **THEN** every panel query resolves to an emitted `ollama_router_*` series and no panel is empty because it queries a series the router never exposes

### Requirement: Tunnel state is visible per node

The fleet dashboards SHALL surface `ollama_router_tunnel_up{node}` so a node whose private share is down is visible without reading logs.

#### Scenario: Tunnel-down node is visible on a dashboard

- **WHEN** a node's zrok private share goes down
- **THEN** a dashboard panel querying `ollama_router_tunnel_up` shows that node with value 0

### Requirement: Readiness reflects whether the fleet can serve

`GET /readyz` SHALL return 503 when there are no healthy non-draining nodes OR when every healthy node is saturated (no healthy node can accept a new request), and 200 otherwise. `GET /healthz` SHALL remain pure liveness and MUST NOT depend on fleet state. The embedding-model gate behavior of `/readyz` is unchanged.

#### Scenario: A saturated-only fleet is not ready

- **WHEN** the only healthy node is at its inflight cap
- **THEN** `/readyz` returns 503 and `/healthz` still returns 200

#### Scenario: Headroom means ready

- **WHEN** at least one healthy non-draining node has inflight below its cap
- **THEN** `/readyz` returns 200

### Requirement: Routing logs name the model

Router-owned rejection and route logs SHALL include the model name whenever the request carries one. Model names are allowlisted; bodies and prompts MUST NOT appear.

#### Scenario: Model miss logs the model

- **WHEN** a chat request misses because no healthy holder has the model
- **THEN** the rejection log line names the model and the reason, with no body content
