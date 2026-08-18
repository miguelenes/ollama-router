## Context

See proposal.md (Why). Today `CreatePodRequest` sends `imageName` from `runpod.image` (PyTorch), `dockerStartCmd: ["bash","-lc", <agent_init.sh>]`, `ports: []`, optional `dataCenterIds`, and secret env. The official `ollama/ollama:latest` image’s entrypoint is `ollama`, so that start command becomes `ollama bash -lc …` and never installs the agent. GitHub release assets for `CARGO_PKG_VERSION` currently 404. Loopback/`scheme-less` enroll and zrok values are accepted by config and then fail from the guest. Specs: `openspec/changes/runpod-ollama-template-e2e/specs/cloud-provider-runpod/spec.md`. Do not change ranking, placement, or `fleet.yaml` GitOps.

## Goals / Non-Goals

**Goals:**

- Make RunPod create produce an enrolled loopback Ollama holder using the official CUDA Ollama runtime, start command, env, and EU-preferred placement with fallback.
- Fail closed on guest-unreachable enroll/zrok URLs before spending GPU time.
- Stop 404 crash-loops; keep FleetState ownership so reconcile can terminate.
- Point the existing fill-and-idle script at the **router**, with phase-0 checks that would have caught the last live test.

**Non-Goals:**

- Do not treat RunPod’s HTTP/TCP proxy as a routing URL (would break `public_url_blocked` and honest tags/infer).
- Do not rewrite `rank_nodes`, job orchestrator, or Verda OAuth.
- Do not vendor a new zrok-instance compose; the operator still runs a guest-reachable controller. Updating the broken `task zrok:fetch` URL is in-scope only as a doc/task pointer, not a new overlay stack in this change.

## Decisions

### D1 — Override entrypoint; keep `bash -lc` bootstrap

Create payload sets `dockerEntrypoint` (exact JSON key confirmed against RunPod REST OpenAPI at apply time) to `/bin/bash` (fallback `/bin/sh` only if the image lacks bash) and keeps `dockerStartCmd` as `["-lc", <script>]` **or** `["bash","-lc", <script>]` with entrypoint `/usr/bin/env`. The script: (1) `OLLAMA_HOST=127.0.0.1 ollama serve` in the background, (2) existing `agent_init.sh` (install agent, zrok private share of loopback `:11434`/`:11436`, enroll). `ports` stay `[]`. `OLLAMA_HOST=0.0.0.0` is not set.

Rejected: start command `["serve"]` only (live smoke works on the proxy, never enrolls). Rejected: stay on PyTorch so `bash` is the entrypoint (user asked for the Ollama NVIDIA CUDA image/template).

### D2 — Image default + optional template id

`runpod.image` default becomes `ollama/ollama:latest`. Optional `runpod.template_id` serializes as `templateId` when non-empty. Image is still sent so create does not depend on the account having a named template (MCP template list was empty in the live test). Operator can pin a digest via `image`.

### D3 — EU preference is a first pass, not a hard allow-list

Add `runpod.preferred_data_centers` defaulting to the current EU ids (`EU-NL-1`, `EU-FR-1`, `EU-CZ-1`, `EU-RO-1`, `EU-SE-1` — refresh from catalog at apply if names drifted). Selector: if `allowed_data_centers` is non-empty, that list is the only filter (existing behavior). Else try preferred ∩ eligible; if empty, retry eligible without the EU filter. Create still sends `dataCenterIds` only when a list is chosen.

Rejected: EU-only hard filter (live stockout would 503 forever).

### D4 — Guest-reachable URL validation at config load

When `runpod.enabled`, require `runpod.enroll_url` and `tunnel.api_endpoint` (after env knobs) to parse as `http://` or `https://`, host not loopback (`127.0.0.1`, `::1`, `localhost`), not a scheme-less IP. Reject RFC1918/link-local enroll hosts (RunPod guests cannot use the home LAN). Do **not** require a live TCP probe at startup (flaky, blocks reboot). E2E phase0 **does** curl those URLs from the operator host as a weak signal; true guest reachability is proven by enroll.

### D5 — Package 404: one-shot failure, then idle wait; reconcile terminates

`agent_init.sh` already exits on download failure; RunPod then restart-loops. On download/setup failure the script MUST log a reason code (no secrets) and `exec sleep infinity` so the container stays `RUNNING` without tight restarts. Reconcile already times out enroll and retains FleetState; it MUST terminate that pod (existing destroy path). Default GitHub URLs remain only when `agent_package_url` is unset **and** the operator has published that version; E2E phase0 HEADs the resolved package URL and aborts before the miss if 404. Overlay/docs tell operators to set `agent_package_url` to a guest-reachable deb/tarball until a GHCR/GitHub release exists.

Rejected: router HEAD at every create (extra latency, still races). Rejected: leaving the 404 restart loop (wastes GPU).

### D6 — E2E script stays router-only

`deploy/swarm/cloud-autoscale-test.sh` phase0 additionally asserts: enroll_url and zrok API are `http(s)` non-loopback; agent package URL is not 404; `runpod/status` 200. Load stays `ROUTER_URL/api/generate`. README MUST NOT document `*.proxy.runpod.net` as the load target. Drain `nuc` remains opt-in so MEDIUM cannot hide on the LAN GPU.

## Risks / Trade-offs

- [Official image has no `bash` / no `apt` / no `curl`] → Mitigation: apply-time check against `ollama/ollama:latest`; if too minimal, wrap start with the image’s shell and install curl via the distro present, or document `image` override to a CUDA image that still runs `ollama`. Do not silently revert to PyTorch in defaults.
- [RunPod JSON uses a different entrypoint field name] → Mitigation: confirm `dockerEntrypoint` vs `containerEntrypoint` on OpenAPI during apply; unit-test `to_json` keys.
- [EU names drift / COMMUNITY has no `dataCenterId`] → Mitigation: fallback path omits EU filter; log allowlisted DC id or `none`.
- [`sleep infinity` on bootstrap failure bills until enroll timeout] → Mitigation: keep enroll timeout bounded (existing create/enroll timeout); terminate on timeout. Better than crash-loop.
- [Guest-reachable zrok still missing on the operator network] → Mitigation: fail closed at config; E2E will not mint pods. Out of scope to stand up public zrok in this change.
- [Default image change surprises operators on PyTorch] → Mitigation: overlay `runpod.image` still overrides; README notes the default flip.

## Migration Plan

1. Overlay: set `runpod.image` / `template_id` as needed; set guest-reachable `enroll_url` and `tunnel.api_endpoint`; set `agent_package_url` if GitHub assets 404; optional `preferred_data_centers`.
2. Redeploy the single router replica (native test port or Swarm overlay). Config error on loopback enroll is expected until URLs are fixed.
3. Run `cloud-autoscale-test.sh` against the router listen URL; undrain `nuc` in the script trap.
4. Rollback: set `runpod.image` back to the PyTorch tag and omit `template_id`; entrypoint override is harmless on PyTorch.

## Open Questions

None that change specs. Entrypoint JSON key and whether `ollama/ollama:latest` includes `bash` are apply-time checks under D1.
