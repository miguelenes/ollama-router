## Why

A capacity miss can already mint a RunPod GPU, but that pod never becomes a healthy fleet holder: bootstrap uses a generic PyTorch image whose GitHub agent assets 404, the official Ollama image’s entrypoint cannot run our `bash -lc` installer, and enroll/zrok URLs are often loopback or scheme-less. Operators therefore cannot finish demand → enroll → infer → idle destroy through the **router**; the only working smoke was hitting RunPod’s public proxy, which this product rejects as `public_url_blocked`.

## What Changes

- **Honest-fleet contract is kept** (list = union, infer = holders, pull = placement, miss = 503, mutates = 501). `auto_pull_on_miss` stays default false. Unknown VRAM stays unknown; known CPU stays `vram_gb: 0` / `gpus: 0`.
- Default RunPod runtime becomes RunPod’s **Ollama NVIDIA CUDA** stack: image `ollama/ollama:latest` (optional `template_id` when the account has that template), operator-set **start command** and **container env**, **preferred European data centers** with fallback when those DCs are stocked out.
- Bootstrap still enrolls a **loopback zrok private share**. The create request MUST override the official image entrypoint so the installer can start `ollama serve` on loopback, install the node-agent from a **guest-reachable** package URL, enable zrok, and `POST /router/v1/nodes/enroll`. `ports` stay empty; RunPod proxy hostnames stay `public_url_blocked`.
- Create is fail-closed when `enroll_url` / `tunnel.api_endpoint` are missing, not `http(s)`, or not reachable from a guest (loopback, RFC1918-only, or a bare IP with no scheme). Agent package 404 MUST NOT crash-loop: the process stays up without tight restarts (not exit-and-restart), MUST surface a reason code, and MUST keep FleetState ownership for teardown.
- Operator E2E: drain local GPU if needed, MEDIUM miss (`llama3.1:8b`) → 503 + `Retry-After` + coalesced create → wait until `origin=runpod` is healthy → concurrent generate **through the router** fills inflight → stop load → idle + grace destroys to the floor. Load is never aimed at `*.proxy.runpod.net`.
- No **BREAKING** client API. Default `runpod.image` change is operator-visible for anyone who relied on the PyTorch default without overriding it.

### Non-Goals

- Thunder remains forbidden.
- No public tunnels: RunPod TCP/HTTP proxy, public `:11434`, and `*.zrok.io` never become healthy routing URLs.
- No RunPod Serverless, Instant Clusters, or adopting foreign (non-managed) Ollama pods.
- No native Hub-pull through one node; miss stays 503 not 404; agent-down stays soft-fail.
- No second router replica / Redis. No Verda OAuth rewrite (Verda stays independently enabled).
- Ranking/placement semantics unchanged.

## Capabilities

### New Capabilities

<!-- none — behavior extends the existing RunPod provider contract -->

### Modified Capabilities

- `cloud-provider-runpod`: Official Ollama image/template, start command + env, EU-preferred placement with stockout fallback, guest-reachable enroll/zrok/agent artifacts, entrypoint-safe bootstrap, fail-closed create, and router-path E2E fill-and-idle.

## Impact

- **Crates**: `crates/ollama-router-runpod` (create DTO: image/template, entrypoint, dockerStartCmd, env, dataCenterIds; bootstrap script; selector preferred DCs); `crates/ollama-router-core` (`runpod:` tunables + validation for template/image/preferred DCs/guest-reachable URLs).
- **Deploy**: `deploy/swarm/router.config.example.yaml`, `deploy/swarm/cloud-autoscale-test.sh`, `deploy/swarm/README.md`; live overlay stays gitignored.
- **Tests**: unit tests for create payload (Ollama image, empty ports, no secrets in cmd, EU preference + fallback, loopback enroll rejected); httpmock create/enroll healthy path; bootstrap does not 404-loop.
- **Docs/skills**: `runpod-pod-fleet` skill, `.opencode/wiki/concepts/ollama-router-node-tunnel.md` as listed in tasks.
- **Sensitivity**: still never log `RUNPOD_API_KEY`, zrok enable token, admin bearer, start-script bodies, or pod env values. Allowlist stays pod id, GPU type, data center, price, VRAM GiB, reason codes.
- **Agent packages**: default GitHub URLs MUST be a published release or operators MUST set `agent_package_url`; unpublished `v0.1.0` is not an acceptable default for live create.
