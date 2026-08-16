## ADDED Requirements

### Requirement: Opt-in bootstrap places desired tiers

When `bootstrap_desired_models` is true, the system SHALL ensure each model in `desired_model_tiers` onto healthy, non-draining nodes that pass label policy and **static** generate-class VRAM for that model (`request-class` on `/api/generate`, not the `/api/pull` path class), intersecting with the tier’s `min_vram_gb` using **known** VRAM only. LARGE MUST NOT target a known CPU. MEDIUM MUST NOT target a known CPU. MEDIUM and LARGE MUST NOT target unknown VRAM. A node with unknown VRAM MUST NOT receive a model from a tier whose `min_vram_gb` is greater than 0. Default `bootstrap_desired_models` MUST remain false: the system MUST NOT start those ensure jobs at serve or reload. Bootstrap MUST NOT block the listen socket. The system MUST NOT log pull bodies.

#### Scenario: bootstrap off does nothing

- **WHEN** `desired_model_tiers` lists `qwen3:8b` and `bootstrap_desired_models` is false
- **THEN** serve and reload do not start an ensure job for that model

#### Scenario: bootstrap LARGE skips CPU and unknown VRAM

- **WHEN** `bootstrap_desired_models` is true, a tier includes `llama3.1:70b`, and the fleet is a known CPU, a known GPU that meets the LARGE estimate, and a node with unknown VRAM
- **THEN** bootstrap ensure targets the known GPU and does not target the CPU or the unknown-VRAM node

#### Scenario: bootstrap respects tier min_vram_gb

- **WHEN** `bootstrap_desired_models` is true, a tier lists `mistral:7b` with `min_vram_gb` 24, and a healthy node has known 8 GiB VRAM
- **THEN** that node is not a bootstrap target for `mistral:7b`

### Requirement: Warm-keeper uses known VRAM and known free

The warm-keeper MUST still only warm models already on disk (it MUST NOT pull). It MUST pick candidates using **known** VRAM against each tier’s `min_vram_gb`; unknown VRAM MAY only warm tiers with `min_vram_gb` equal to 0. It MUST skip for free VRAM only when free VRAM is **known** and below `model_warm_min_free_vram_gb`. Omitted free VRAM or omitted total VRAM MUST NOT encode as `0` and MUST NOT skip.

#### Scenario: warm-keeper still does not pull

- **WHEN** `bootstrap_desired_models` is false, a healthy GPU is generate-class-eligible for a desired-tier model, and that model is not on disk
- **THEN** the warm-keeper does not pull it (it only warms models already on disk)

#### Scenario: warm-keeper does not treat unknown free as zero

- **WHEN** a healthy node has a desired-tier model on disk, unknown VRAM, unknown free VRAM, and `min_vram_gb` 0 for that model
- **THEN** the warm-keeper MAY warm it (it MUST NOT skip as if free VRAM were 0)

#### Scenario: warm-keeper unknown VRAM stays off GPU-only tiers

- **WHEN** a healthy node has unknown VRAM and `mistral:7b` on disk in a tier with `min_vram_gb` 24
- **THEN** the warm-keeper does not select that model (unknown is not a 24 GiB GPU)

### Requirement: Known insufficient disk skips a pull target

The system SHALL skip a pull/ensure target when that node’s disk free is **known** and is below the pull size estimate. The estimate MUST be that **target node’s** tags-probe `size` when present and greater than zero; otherwise the aggregated catalog `size` for the model when present and greater than zero (GiB = bytes / 1024³). When both are omitted or zero, the system MUST NOT skip for disk. When disk free is unknown (agent down or omitted), the system MUST NOT encode it as `0` and MUST NOT skip for disk. Inference ranking MUST NOT use disk free. Skips MUST NOT log bodies or persist provider `detail` strings.

#### Scenario: known low disk is skipped

- **WHEN** a healthy GPU is otherwise placement-eligible for `qwen3:8b`, the catalog `size` is 5 GiB, and the agent reported 1 GiB disk free
- **THEN** that GPU is not a remaining pull target (skipped for disk)

#### Scenario: unknown disk stays eligible

- **WHEN** a healthy GPU is otherwise placement-eligible for `qwen3:8b`, the catalog `size` is 5 GiB, and disk free is unknown
- **THEN** that GPU remains a pull target
