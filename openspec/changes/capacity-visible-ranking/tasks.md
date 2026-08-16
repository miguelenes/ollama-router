## 1. Known vs zero capacity

- [x] 1.1 Add routing-facing accessors so omitted `vram_gb` / `gpus` stay `None` (YAML `0` remains a measured CPU). Do not silently change every `Capacity::vram_gb()` metrics caller
- [x] 1.2 Update `vram_fits` / `static_capacity_fits`: MEDIUM and LARGE require `Some(vram)` meeting existing thresholds; `None` fails those classes only; EMBED/SMALL/GENERIC stay admissible on unknown VRAM
- [x] 1.3 Update `capacity_preference`: SMALL bands are known GPU (`gpus >= 1`), then unknown, then known CPU (`Some(0)`). EMBED/MEDIUM MUST NOT treat omitted VRAM as `0`. Utilization still dominates
- [x] 1.4 Fix rank unit tests: CPU fixtures set `vram_gb: Some(0)` / `gpus: Some(0)`; add unknown-vs-GPU SMALL preference, EMBED and MEDIUM known-8GiB over unknown, and LARGE-unknown-does-not-fit cases. Keep existing WLC tests (utilization beats preference, saturated never selected)

## 2. Request class from the catalog

- [x] 2.1 Parse Ollama `details.parameter_size` (`1B`, `1.2B`, `8.0B`) into billions with the same thresholds as `:Nb`
- [x] 2.2 Extend classify with an optional size hint used only when `:Nb` is absent (after embed markers and known-small bases). Untagged + no hint stays MEDIUM. `/v1/completions` uses the same size class as generate
- [x] 2.3 Proxy and placement (`placement_class`) pass the hint from a healthy holder’s `TagRecord` (prefer aggregated-tag winner details). Do not log `details`
- [x] 2.4 Unit tests: `minicpm-v4.6:latest` + `1B` → SMALL; `llama3.1:70b` stays LARGE if details disagree; no hint → MEDIUM; `qwen3:30b-a3b` stays LARGE; show stays GENERIC

## 3. Placement pool and inventory

- [x] 3.1 Set static `capacity.vram_gb` / `gpus` on `deploy/fleet.local.yaml` `local` with comments (`0`/`0` = known CPU; omit = unknown). Add `gpus: 1` on mock GPU in `deploy/fleet.yaml` if missing
- [x] 3.2 Placement/orchestrator test: LARGE ensure/pull default (healthy) placement omits known CPU and unknown-VRAM nodes and includes a known GPU that meets the LARGE estimate; MEDIUM default placement omits a known CPU and includes a GPU that meets the medium VRAM minimum
- [x] 3.3 Proxy test: LARGE generate against a holder with unknown VRAM returns 503 `insufficient_capacity` and is not forwarded; SMALL still forwards to an unknown-VRAM holder. Keep existing miss-503 and stream-retry tests

## 4. Docs and gate

- [x] 4.1 Product wiki + `ollama-compat-proxy` + `routing-wlc` skills (`.cursor` and `.opencode`): honest fleet contract; holders-only WLC; pull places on healthy generate-class-eligible nodes; unknown VRAM is not a CPU; pull is not a stub
- [x] 4.2 Run `task check`
