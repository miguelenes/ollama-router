//! Utilization weighted least-connections + class preference (pure fns).

mod classify;
mod error;
mod place;
mod rank;

pub use classify::{
    classify, classify_path, classify_with_markers, classify_with_size_hint, is_inference_path,
    is_known_small_base, looks_like_embedding, parse_model_size_b, parse_parameter_size_b,
    size_hint_from_tag_details, RequestClass, DEFAULT_EMBED_MARKERS, DEFAULT_SMALL_MODEL_BASES,
};
pub use error::RoutingError;
pub use place::{
    placement_class, placement_eligible_node_ids, ram_pressure_blocks_placement,
    resolve_target_nodes, size_hint_from_catalog, PlacementError, TargetSpec, TARGET_ALL,
};
pub use rank::{
    blocked_only_by_reservations, capacity_fits, estimate_large_vram_gb, estimate_request_ram_gb,
    estimate_request_vram_gb, load_key, ram_fits, rank_nodes, static_capacity_fits, vram_fits,
    RankOutcome,
};
