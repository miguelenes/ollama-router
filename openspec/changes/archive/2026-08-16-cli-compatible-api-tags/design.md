## Context

See proposal.md (Why). Today health probes parse `/api/tags` down to names (`TagModel { name }`) and `Registry::update_models` stores `HashSet<String>`. `aggregated_tags()` returns `(name, node_ids)`; the proxy serializes `name` / `model` / `details.router_nodes` only. Official `ollama list` does `m.Digest[:12]` with no length check.

Constraints: keep routing `has_model` on the name set; do not log tag bodies; do not treat tags as idle; GiB/metrics unlabeled by model name stay as they are. Probe bodies remain capped (`health.max_probe_body_bytes`).

## Goals / Non-Goals

**Goals:**

- Persist enough of each node's list record to emit a CLI-safe `/api/tags`.
- Merge by normalized name with a deterministic digest winner.
- Keep `update_models(names)` working for tests; name-only rows still MUST serialize a ≥12-char digest.

**Non-Goals:**

- Aggregating `GET /api/ps` (still one ranked node).
- Changing pull/delete job recovery (names are enough).
- Fixing the upstream Ollama CLI bounds check.
- Storing or returning model files, blobs, or `/api/show` payloads in the catalog.

## Decisions

### 1. Per-node tag records beside the name set

Keep `Cold.models: HashSet<String>` as the routing source of truth (`has_model`, placement, metrics counts). Add `Cold.tag_records: HashMap<String, TagRecord>` keyed by normalized name.

`TagRecord` holds `digest`, `size`, `modified_at` (RFC3339 string as received), `details` (`serde_json::Value`, ignore unknown), `capabilities` (`Vec<String>`). Serde on the probe: ignore extras; do not `deny_unknown_fields`.

On a successful tags probe, replace both the name set and the record map for that node. Drop records whose names disappeared from the probe.

**Alternative considered:** Replace the name set with records only. Rejected — every `has_model` call site and test helper would grow; names are the load-bearing identity.

**Alternative considered:** Re-fetch `/api/tags` on each client `GET`. Rejected — extra upstream load, and aggregation would still need a merge; health already polls every ~5s.

### 2. Always emit `digest` of length ≥ 12

Prefer the probe digest when `len >= 12`. Otherwise emit a stable hex placeholder: lowercase hex of SHA-256(normalized name), 64 characters (or at least 12). Never omit `digest`, never emit `""`.

**Alternative considered:** Dummy `"000000000000"`. Rejected — every incomplete row would share one ID in `ollama list`.

**Alternative considered:** Omit models without a real digest. Rejected — tests and a partial parse would hide models that routing still considers present.

### 3. Conflict merge: newest `modified_at`, then node id

Parse `modified_at` as RFC3339 when present. Missing/unparseable timestamps sort last. Tie-break: lexicographically smaller `NodeId`. `router_nodes` is the sorted list of all healthy holders, independent of which record won.

**Alternative considered:** Duplicate names (one row per digest). Rejected — `ollama list` would show the same NAME twice; clients key on name.

**Alternative considered:** Prefer the node that currently has the model loaded (`/api/ps`). Deferred — warmth is a routing tie-break, not catalog identity; can be added later without changing the spec's "one row per name" rule.

### 4. Proxy serializes records; OpenAI `created` from `modified_at`

`aggregated_tags` returns structured rows (name, nodes, record). The proxy maps them to Ollama JSON, merging `details.router_nodes` into `details` (create `details` if absent). OpenAI list/retrieve: `created` = Unix seconds from the winning `modified_at` when parseable, else `0`.

### 5. Test helpers

Extend `mark_ready` (or add `mark_ready_with_tags`) so proxy tests can inject digest/size/`modified_at`. Name-only `update_models` remains valid and MUST still produce a ≥12-char digest via the placeholder rule.

## Risks / Trade-offs

- [CLI still panics on other fields] → Spec only guarantees `digest` length; `size` 0 and zero time already render as `0 B` / `Never`. Add a live `OLLAMA_HOST` `ollama list` check in the apply notes, not CI (no ollama binary pin).
- [Stale digest after pull] → Next tags probe (~5s) replaces the record. Jobs already recover via live `/api/tags` names.
- [Memory] → One record per model per node; bounded by probe cap. No raw body retained.
- [SHA-256 placeholder mistaken for a blob id] → Document in wiki; real probes always have 64-char Ollama digests so production LAN fleets will not hit this path.

## Migration Plan

- Deploy router binary; no fleet.yaml or SQLite migration.
- First successful tags probe per node fills records. Until then, name-only state (fresh process) still lists models with placeholder digests if `update_models` ran, or empty catalog if no probe yet (same as today).
- Rollback: previous binary returns name-only tags; CLI panics again. No data format to revert.

## Open Questions

None. Placeholder digest vs omit-row is decided (always emit ≥12 chars).
