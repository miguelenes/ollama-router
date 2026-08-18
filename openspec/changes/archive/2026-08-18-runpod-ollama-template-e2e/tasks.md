## 1. Config tunables and fail-closed URLs

- [x] 1.1 Change default `runpod.image` to `ollama/ollama:latest` in `router.defaults.yaml` and `RunpodConfig`; add optional `template_id` and `preferred_data_centers` (default EU ids from design D3). Overlay example documents the image flip and `agent_package_url`
- [x] 1.2 When `runpod.enabled`, reject loopback / RFC1918 / scheme-less `enroll_url` and `tunnel.api_endpoint` (spec: "Loopback enroll_url is rejected", "Bare IP tunnel endpoint is rejected"); accept guest-reachable `http(s)` enroll (spec: "Guest-reachable enroll is accepted"). No secrets in YAML
- [x] 1.3 Unit tests in `config/load.rs` for those three URL scenarios plus enabled-without-key still fail-closed; `thunder:` still unknown-field

## 2. Create payload: Ollama runtime, entrypoint, template

- [x] 2.1 Confirm RunPod REST create field for entrypoint (`dockerEntrypoint` vs OpenAPI name). Extend `CreatePodRequest` + `to_json`: default image, empty `ports`, optional `templateId`, entrypoint override (design D1). Do not SSH
- [x] 2.2 `startup.rs`: start command is not image-default `serve` alone; script starts `OLLAMA_HOST=127.0.0.1 ollama serve` then existing agent init. Secrets only in env. Tests: spec "Default create uses the official Ollama image", "Template id is honored when configured", "Secrets stay in env, not the start command" (Debug redaction stays)

## 3. Bootstrap: 404 must not crash-loop

- [x] 3.1 `agent_init.sh`: on package 4xx/5xx or missing URL, log a reason code (no tokens) and `exec sleep infinity` (design D5). Tests or script assertions for spec "Package 404 does not crash-loop"
- [x] 3.2 Manager: enroll timeout still retains `managed_by=runpod` and terminates (existing destroy path). httpmock: failed bootstrap / never-enroll → terminate, ownership kept until success

## 4. EU preference with fallback

- [x] 4.1 Selector: if `allowed_data_centers` non-empty, hard filter only (spec: "Hard allow-list is not bypassed"); else prefer `preferred_data_centers` then retry without (spec: "EU offer wins when in stock", "EU stockout falls back")
- [x] 4.2 Unit tests for those three scenarios; do not treat unknown VRAM as 0 (existing value ranking unchanged)

## 5. Tunnel-only health (existing contract)

- [x] 5.1 Keep tests that loopback zrok enroll becomes healthy and RunPod proxy / public IP is `public_url_blocked` (spec: "Enrolled loopback share becomes healthy", "Public pod URL is blocked"). Do not add a public-proxy routing path

## 6. Operator E2E through the router

- [x] 6.1 Update `deploy/swarm/cloud-autoscale-test.sh` phase0: `runpod/status` 200; enroll_url and zrok API are `http(s)` non-loopback; HEAD agent package URL not 404. Load stays `${ROUTER_URL}/api/generate` (spec: "MEDIUM miss becomes a healthy RunPod holder", "Subsequent generate is served through the router", "Idle above floor terminates"). Capacity miss still 503 + `Retry-After` (existing proxy test stays green)
- [x] 6.2 `deploy/swarm/README.md` + `router.config.example.yaml`: guest-reachable enroll/zrok, `agent_package_url`, Ollama image/template, EU preference; never document `*.proxy.runpod.net` as the load target. Drain `nuc` remains opt-in. Note `task zrok:fetch` upstream URL drift (zrok2-instance) without vendoring a new stack

## 7. Docs, skills, gate

- [x] 7.1 Update `.cursor/skills/runpod-pod-fleet/SKILL.md` and `.opencode/skills` / `.agents/skills` copies if present; `.opencode/wiki/concepts/ollama-router-node-tunnel.md` for Ollama image + entrypoint + guest-reachable enroll
- [x] 7.2 Sequential `task check` (`cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace --locked`, `cargo deny`) then `task coverage` (≥80% lines, ignore `**/main.rs`). No Makefile/justfile/npm scripts
