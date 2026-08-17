## Why

Woodpecker `swarm-deploy` updated the stack and listed the service but did not prove the new task was live. A bad deploy could leave the pipeline green until an operator manually curled `/healthz`.

## What Changes

- Extend `.woodpecker/swarm-deploy.yml` to confirm the service image includes the pinned `sha-<git>` tag and poll `GET /healthz` on the running Swarm task before the job succeeds.
- Poll timeout ~4 minutes (120×2s), tolerating `stop-first` rollout gaps with no running task briefly.
- Document the verification step in `deploy/swarm/README.md` and `deploy/woodpecker/README.md`.
- Probe uses in-container curl via `docker exec` on the Swarm task (Woodpecker step containers do not share the manager host netns). Liveness only — not `/readyz` (fleet may have no healthy nodes on fresh bootstrap).

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `swarmnet-local-deploy`: swarm-deploy SHALL verify the deployed SHA tag and `/healthz` before completing.

## Impact

- `.woodpecker/swarm-deploy.yml` — post-deploy tag check + healthz poll with failure diagnostics.
- `deploy/swarm/README.md`, `deploy/woodpecker/README.md` — CI flow wording.
- No Rust crates, stack shape, or honest-fleet proxy changes.
