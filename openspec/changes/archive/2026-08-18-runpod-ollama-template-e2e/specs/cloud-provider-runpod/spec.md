## ADDED Requirements

### Requirement: Official Ollama NVIDIA CUDA runtime
When RunPod is enabled, new pods SHALL use RunPod’s Ollama NVIDIA CUDA runtime: default image `ollama/ollama:latest`, or the operator’s configured `template_id` when set. The create request SHALL carry a start command that keeps Ollama serving on loopback and SHALL pass operator-configured container environment variables (enable token and enroll bearer only via named env vars, never in the start-command string). The official image entrypoint (`ollama`) MUST be overridden so the bootstrap start command can run; the router MUST NOT SSH. `ports` MUST remain empty so the RunPod proxy is not published.

#### Scenario: Default create uses the official Ollama image
- **WHEN** RunPod scale-up creates a pod and `template_id` is unset
- **THEN** the create uses image `ollama/ollama:latest`, publishes no ports, and the start command is not the image default `serve` alone

#### Scenario: Template id is honored when configured
- **WHEN** the overlay sets a non-empty `runpod.template_id`
- **THEN** the create includes that template id and still publishes no ports

#### Scenario: Secrets stay in env, not the start command
- **WHEN** a pod is created with zrok enable token and admin enroll bearer present in the process environment
- **THEN** those values appear only as container env under the configured env-var names and MUST NOT appear in the start-command string or logs

### Requirement: Preferred European data centers with stockout fallback
The system SHALL prefer European RunPod data centers when creating pods. When every preferred European data center reports no eligible availability for the selected GPU types, the create MUST fall back to other eligible data centers that still pass VRAM band, allow/deny lists, and the hourly price cap, rather than failing as a hard EU-only stockout. An explicit non-empty `allowed_data_centers` list remains a hard filter and MUST NOT be bypassed by the EU preference.

#### Scenario: EU offer wins when in stock
- **WHEN** an eligible GPU is available in a preferred European data center and also outside Europe at equal or worse value
- **THEN** the create requests the European data center

#### Scenario: EU stockout falls back
- **WHEN** preferred European data centers have no eligible availability and a non-EU eligible offer remains under the price cap
- **THEN** the pod is still created on that fallback offer

#### Scenario: Hard allow-list is not bypassed
- **WHEN** `allowed_data_centers` is a non-empty list that excludes the fallback data center
- **THEN** that fallback is not requested

### Requirement: Guest-reachable enroll and zrok controller
When RunPod is enabled, `runpod.enroll_url` and the tunnel API endpoint MUST be absolute `http://` or `https://` URLs that a cloud guest can reach. Loopback hosts, scheme-less IPs, and RFC1918-only enroll URLs MUST be rejected at startup or before create with a configuration/reason error. A successful enroll still MUST be a loopback zrok **private** share; the enroll **URL** (router admin) is the guest-reachable exception.

#### Scenario: Loopback enroll_url is rejected
- **WHEN** `runpod.enroll_url` is `http://127.0.0.1:11437` (or another loopback host) and RunPod is enabled
- **THEN** startup or create fails closed with an error naming `enroll_url`, and no pod is created

#### Scenario: Bare IP tunnel endpoint is rejected
- **WHEN** the tunnel API endpoint is a host or IP with no `http://` or `https://` scheme
- **THEN** configuration loading fails with an error naming the tunnel API endpoint field

#### Scenario: Guest-reachable enroll is accepted
- **WHEN** `runpod.enroll_url` is an `https://` (or `http://`) URL whose host is not loopback, not RFC1918, and not link-local
- **THEN** RunPod create may proceed (credentials and other tunables permitting)

### Requirement: Agent package fetch failure does not crash-loop
The bootstrap MUST install the node-agent from a guest-reachable `agent_package_url` or from published release assets. If every configured package URL returns HTTP 4xx/5xx or is unset/unpublished, the container MUST NOT tight-restart: it MUST log a reason code without secrets, keep the process running without retrying the download in a loop, and retain the FleetState ownership row so reconcile can terminate the pod. Unpublished GitHub `v0.1.0` assets MUST NOT be treated as a working default.

#### Scenario: Package 404 does not crash-loop
- **WHEN** the agent package URLs all return HTTP 404 after pod start
- **THEN** bootstrap does not tight-restart, logs a reason code with no token material, and the `managed_by=runpod` FleetState row remains until terminate succeeds

### Requirement: Router-path fill-and-idle completes
After a client capacity miss for a MEDIUM model that the permanent GPU fleet cannot serve, the system SHALL create a RunPod pod, wait until that node is enrolled and healthy as `origin=runpod`, and then forward subsequent generate/chat for that model through the **router listen URL**. Concurrent client streams on that URL SHALL count toward inflight saturation. After client forwards stop and idle timeout plus post-create grace elapse (above the floor), the pod SHALL be terminated permanently. Load MUST NOT be required against `*.proxy.runpod.net`. `fleet.yaml` hosts MUST NOT be destroyed.

#### Scenario: MEDIUM miss becomes a healthy RunPod holder
- **WHEN** a generate for `llama3.1:8b` misses capacity and RunPod is enabled below its ceiling with guest-reachable enroll, zrok, and agent package
- **THEN** the client receives 503 with `Retry-After`, one coalesced pod is created, and after enroll the admin nodes listing shows a healthy `origin=runpod` row whose URL is not a RunPod proxy hostname

#### Scenario: Subsequent generate is served through the router
- **WHEN** that RunPod node is healthy and a client POSTs `/api/generate` for `llama3.1:8b` to the router listen URL
- **THEN** the request is forwarded to the enrolled loopback share (not to `*.proxy.runpod.net`) and streams as NDJSON

#### Scenario: Idle above floor terminates
- **WHEN** no client forwards have occurred for `idle_timeout` after post-create grace, and the managed count is above `min_instances`
- **THEN** the pod is terminated permanently and `cloud_instances` for RunPod returns to the floor

## MODIFIED Requirements

### Requirement: Container bootstrap with tunnel-only URLs
RunPod pods SHALL be bootstrapped by running the configured Ollama NVIDIA CUDA image (or template) with an operator-visible start command and container environment that carry the node-agent enroll configuration and zrok material; the router MUST NOT SSH into pods. The start command MUST launch Ollama bound to loopback, enable a zrok **private** share of that loopback port, and enroll via `POST /router/v1/nodes/enroll`. The pod's Ollama URL becomes healthy only after enroll of that loopback share. The pod's public IP, RunPod proxy hostname (`*.proxy.runpod.net`), or any non-loopback URL MUST be rejected as `public_url_blocked` and never become healthy. Enroll MUST NOT write `fleet.yaml`.

#### Scenario: Enrolled loopback share becomes healthy
- **WHEN** a pod's node-agent enrolls a loopback zrok private share URL
- **THEN** the node becomes routable after health checks pass

#### Scenario: Public pod URL is blocked
- **WHEN** an enroll or state row carries the pod's public IP or a RunPod proxy hostname
- **THEN** the URL is rejected as `public_url_blocked` and the node is never marked healthy
