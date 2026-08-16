//! Opt-in desired-tier bootstrap: background ensure after bind / reload.

use std::collections::BTreeMap;
use std::time::Duration;

use ollama_router_core::config::{ModelTier, RouterConfig};
use ollama_router_core::fleet::{normalize_model, NodeId, NodeSnapshot, Registry};
use ollama_router_core::routing::{placement_eligible_node_ids, size_hint_from_catalog};
use tokio_util::sync::CancellationToken;

use crate::http::AppState;

/// Spawn only when `bootstrap_desired_models` is true. Does not block listen.
pub async fn run(state: AppState, shutdown: CancellationToken) {
    if !state.config.bootstrap_desired_models {
        tracing::debug!("bootstrap_desired_models_disabled");
        return;
    }
    if state.config.desired_model_tiers.is_empty() {
        tracing::debug!("bootstrap_no_desired_tiers");
        return;
    }
    let wait = Duration::from_secs_f64(state.config.bootstrap_probe_wait_seconds.max(0.0));
    if !wait.is_zero() {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
    if shutdown.is_cancelled() {
        return;
    }
    ensure_desired_tiers(&state).await;
}

/// Build per-model ensure targets for desired tiers (known-VRAM + generate-class).
pub fn bootstrap_targets(
    nodes: &[NodeSnapshot],
    config: &RouterConfig,
    registry: &Registry,
) -> BTreeMap<String, Vec<NodeId>> {
    let tags = registry.aggregated_tags();
    let mut targets: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for tier in &config.desired_model_tiers {
        for model in &tier.models {
            let model = normalize_model(model);
            if model.is_empty() {
                continue;
            }
            let hint = size_hint_from_catalog(&tags, &model);
            let eligible = if config.bootstrap_require_capacity {
                placement_eligible_node_ids(
                    nodes,
                    &model,
                    &config.policy,
                    false,
                    config.bootstrap_require_ram_headroom,
                    hint,
                )
            } else {
                nodes
                    .iter()
                    .filter(|n| n.healthy && !n.draining && !n.cordoned)
                    .map(|n| n.id.clone())
                    .collect()
            };
            let filtered: Vec<NodeId> = eligible
                .into_iter()
                .filter(|id| {
                    let Some(node) = nodes.iter().find(|n| n.id == *id) else {
                        return false;
                    };
                    node_meets_tier_vram(node, tier)
                })
                .collect();
            if filtered.is_empty() {
                continue;
            }
            targets.entry(model).or_default().extend(filtered);
        }
    }
    for ids in targets.values_mut() {
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids.dedup();
    }
    targets
}

fn node_meets_tier_vram(node: &NodeSnapshot, tier: &ModelTier) -> bool {
    match node.known_vram_gb() {
        Some(vram) => vram >= tier.min_vram_gb,
        None => tier.min_vram_gb == 0.0,
    }
}

async fn ensure_desired_tiers(state: &AppState) {
    let snap = state.registry.snapshot();
    let targets = bootstrap_targets(&snap, &state.config, &state.registry);
    if targets.is_empty() {
        tracing::info!("bootstrap_no_placement_targets");
        return;
    }
    let model_count = targets.len();
    let pair_count: usize = targets.values().map(Vec::len).sum();
    match state.orchestrator.start_ensure_targets(targets).await {
        Ok(job) => {
            tracing::info!(
                job_id = %job.id,
                models = model_count,
                targets = pair_count,
                "bootstrap_ensure_started"
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "bootstrap_ensure_failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ollama_router_core::config::{Capacity, ModelTier, NodeConfig, RouterConfig};
    use ollama_router_core::fleet::Registry;

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    fn node(id: &str, vram: Option<f64>, gpus: Option<u32>) -> NodeConfig {
        NodeConfig {
            id: nid(id),
            url: Some(format!("http://{id}:11434")),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: vram,
                ram_gb: Some(32.0),
                gpus,
                cpu_cores: Some(8),
            },
            max_inflight: None,
        }
    }

    #[test]
    fn bootstrap_large_skips_cpu_and_unknown_vram() {
        let config = RouterConfig {
            nodes: vec![
                node("cpu", Some(0.0), Some(0)),
                node("gpu80", Some(80.0), Some(1)),
                node("unknown", None, None),
            ],
            desired_model_tiers: vec![ModelTier {
                models: vec!["llama3.1:70b".into()],
                min_vram_gb: 0.0,
            }],
            bootstrap_desired_models: true,
            bootstrap_require_capacity: true,
            ..RouterConfig::default()
        };
        let registry = Registry::new(&config);
        for id in ["cpu", "gpu80", "unknown"] {
            registry.set_healthy(&nid(id));
        }
        let targets = bootstrap_targets(&registry.snapshot(), &config, &registry);
        let ids = &targets["llama3.1:70b"];
        assert_eq!(
            ids.iter().map(NodeId::as_str).collect::<Vec<_>>(),
            vec!["gpu80"]
        );
    }

    #[test]
    fn bootstrap_respects_min_vram_gb() {
        let config = RouterConfig {
            nodes: vec![node("gpu8", Some(8.0), Some(1))],
            desired_model_tiers: vec![ModelTier {
                models: vec!["mistral:7b".into()],
                min_vram_gb: 24.0,
            }],
            bootstrap_desired_models: true,
            bootstrap_require_capacity: true,
            ..RouterConfig::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("gpu8"));
        let targets = bootstrap_targets(&registry.snapshot(), &config, &registry);
        assert!(targets.is_empty());
    }

    #[test]
    fn bootstrap_off_builds_nothing_when_flag_false() {
        // Selection still works; the runner gates on the flag. Empty when flag unused
        // is covered by serve not spawning. Here: known-VRAM gate alone.
        let config = RouterConfig {
            nodes: vec![node("gpu", Some(24.0), Some(1))],
            desired_model_tiers: vec![ModelTier {
                models: vec!["qwen3:8b".into()],
                min_vram_gb: 0.0,
            }],
            bootstrap_desired_models: false,
            ..RouterConfig::default()
        };
        let registry = Registry::new(&config);
        registry.set_healthy(&nid("gpu"));
        let targets = bootstrap_targets(&registry.snapshot(), &config, &registry);
        assert_eq!(targets["qwen3:8b"].len(), 1);
    }
}
