# Legacy compatibility inventory

| Surface | File | Action | Migration / replacement |
| --- | --- | --- | --- |
| FleetState `extra.thunder_instance_id` | `crates/ollama-router-core/src/fleet/state.rs` | Remove | Strip on load/persist; no replacement |
| FleetState `extra.tailscale_ip` | `crates/ollama-router-core/src/fleet/state.rs` | Remove | Strip on load/persist; use zrok private share enroll |
| Jobs `TargetStatus` unknown → `Failed` shim | `crates/ollama-router-core/src/jobs/types.rs` | Remove | Use current status strings only |
| Windows schtasks legacy cleanup | `crates/ollama-node-agent/src/setup/windows.rs` | Remove | SCM service stop only |
| YAML `thunder:` tunables block | `crates/ollama-router-core/src/config/load.rs` | Reject (existing) | Use `verda:` and/or `runpod:` |
| Env `THUNDER_*` variables | `crates/ollama-router-core/src/config/load.rs` | Reject | Use Verda or RunPod env/config |
| Unknown HTTP routes | `crates/ollama-router/src/proxy/mod.rs` | Fail with `unknown_path` + hint | Use documented Ollama-native / OpenAI-compatible endpoints |
| Historical "capacity-agent" terminology | router crates + node-agent packaging | Rename comments/errors | Use "node-agent" |

**Kept (not legacy):** `sticky_affinity`, `capacity_url`, zrok enroll fields (`ollama_share_id`, `agent_share_id`), `/api/embeddings` → `/api/embed` rewrite (current Ollama contract).
