# Local skill-usage analytics wiring

This repo now records skill invocations into the **agent-config** local
workspace event log, so `/skills discover`'s analytics-backed classes
(`recently-adopted`, `popular-in-role`) have data to rank on.

## How it's wired

- **Plugin:** `.opencode/plugins/skills-analytics.js` — an OpenCode server
  plugin with a `tool.execute.after` hook that fires when the `skill` tool is
  invoked.
- **Registration:** root `opencode.json` → `"plugin": ["./.opencode/plugins/skills-analytics.js"]`.
- **Output:** appends one JSONL line to
  `~/.event4u/agent-config/workspace/analytics/events.jsonl` (created on first
  write). This is the path the `/analytics` cluster and `/skills discover`
  recommender read — **not** a repo-local file.

## Record shape

Field names are confirmed from `/analytics show` (CSV columns):

```jsonl
{"ts":"2026-08-17T00:45:00.000Z","event":"launch","role":null,"task":"routing-wlc","host_tier":null,"duration_ms":1234}
```

| field        | source                                             | status                |
|--------------|----------------------------------------------------|-----------------------|
| `ts`         | `new Date().toISOString()`                         | confirmed             |
| `event`      | constant `"launch"`                                | **unconfirmed** ⚠     |
| `role`       | `OLLAMA_ROUTER_ANALYTICS_ROLE` env, else `null`    | **unconfirmed** ⚠     |
| `task`       | skill id from `args.name`                          | confirmed             |
| `host_tier`  | `null`                                             | **unconfirmed** ⚠     |
| `duration_ms`| `tool.execute.before`→`after` delta (per `callID`) | confirmed (0 if unknown) |

## ⚠ Unconfirmed tokens

The contract `docs/contracts/local-analytics.md` § Event vocabulary is **not
installed** on this machine, and the vocabulary is **closed** ("emitters reject
unknown event names"). Until that contract is vendored, `event`, `role`, and
`host_tier` are best-effort placeholders. Reconcile them in
`.opencode/plugins/skills-analytics.js` (all three are single obvious
constants/assignments) once the contract is available.

## Opt-out

The hook short-circuits when `AGENT_CONFIG_NO_LOCAL_ANALYTICS=1`, mirroring the
recommender's kill-switch. `analytics.local: off` in `.agent-settings.yml`
would need to be checked by the recommender itself; the hook only honours the
env var today.

## Idempotency

A `seen` set keyed by `callID` prevents double-append if the plugin is ever
registered twice (e.g. auto-discovery plus explicit config). Analytics is
best-effort: append failures are swallowed and never break the tool pipeline.
