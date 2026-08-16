## Why

The router is meant to be one URL over a cluster: list the union, send generate/chat/embed to the best **holder** by size class and load. Ranking and placement still treat omitted VRAM/GPU as **zero**, so a healthy GPU with no static `capacity` and a silent agent looks like a CPU — LARGE is `insufficient_capacity`, SMALL cannot prefer GPU, and LARGE **pulls skip that GPU**. Untagged / `:latest` names class as MEDIUM even when tags already reported `details.parameter_size`. Core WLC and placement-to-all-eligible-nodes exist in code but were never specified.

## What Changes

- Specify **inference routing** that is already largely implemented: holders only, utilization-first WLC, class VRAM preference, saturation 503, stream with pre-first-byte retry, miss 503 `model_missing`.
- Specify **model placement**: default pull/ensure targets every **healthy** node that fits the model’s **generate** class (not `RequestClass::Pull`; `#all` may widen). LARGE/MEDIUM skip unknown VRAM the same way ranking does.
- Specify **request class** (name markers, exact small bases, `:Nb` thresholds, then `parameter_size`, else MEDIUM). Path-only embed/show/pull unchanged.
- Ranking, placement, and class gates MUST distinguish **unknown** capacity from a measured CPU (`vram_gb = 0`, `gpus = 0`). Unmeasured VRAM MUST NOT be encoded as `0` for those gates, SMALL GPU preference, or EMBED/MEDIUM “lower VRAM” sort. Unknown MAY share the lowest-tier inflight cap number.
- MEDIUM and LARGE inference (and placement) MUST require **known** VRAM that meets existing static gates; unknown does not fit (503 `insufficient_capacity` on infer). EMBED / SMALL / GENERIC stay admissible on unknown VRAM when other filters pass.
- Local-dev `fleet.local.yaml` declares static VRAM/GPUs; mock GPU gets `gpus: 1` if missing.
- Docs/wiki and `ollama-compat-proxy` / `routing-wlc` skills: honest fleet (list = union, infer = holders, pull = placement job, miss = 503, 501 mutates); unknown VRAM is not a CPU; pull is not a stub.
- Not in this change: native pull NDJSON, default `auto_pull_on_miss`, 404-on-miss, fleet-union `/api/ps`, GPU-util in the sort key, making agent-down nodes unhealthy.

## Capabilities

### New Capabilities

- `size-load-routing`: Omitted VRAM/GPU is unknown, not a silent CPU. MEDIUM/LARGE need known sufficient VRAM for ranking and placement gates.
- `inference-routing`: Generate/chat/embed (and OpenAI inference) rank among holders by load then class preference; stream; 503 on miss/saturation/capacity.
- `model-placement`: Default pull/ensure places the model on every **healthy** generate-class-eligible node so load-share has a pool.
- `request-class`: Size class from path, name markers, `:Nb`, then tags-probe `parameter_size`.

### Modified Capabilities

- (none — `openspec/specs/` has no archived capabilities yet; `api-tags` stays in `cli-compatible-api-tags`)

## Impact

- `crates/ollama-router-core/src/routing/rank.rs` — Option VRAM/GPU, SMALL bands, EMBED/MEDIUM known-VRAM preference, MEDIUM/LARGE gates (WLC sort key stays).
- `crates/ollama-router-core/src/routing/place.rs` — `static_capacity_fits` / `placement_class` inherit known-VRAM gates and class hint.
- `crates/ollama-router-core/src/routing/classify.rs` + proxy — `parameter_size` hint.
- `crates/ollama-router-core/src/config/models.rs` / fleet snapshots — routing accessors; do not blindly change metrics `vram_gb()`.
- `deploy/fleet.local.yaml`, `deploy/fleet.yaml` (mock GPU `gpus`).
- Tests: unknown vs GPU, EMBED known-8GiB over unknown, LARGE unknown 503, classify `:latest`, placement skips unknown LARGE, existing WLC tests kept.
- Wiki + `ollama-compat-proxy` + `routing-wlc` (`.cursor` and `.opencode`).
- Sensitivity unchanged.
