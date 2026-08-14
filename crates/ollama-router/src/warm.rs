//! Background model warm-keeper: occupy inflight without resetting idle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ollama_router_core::fleet::{NodeId, NodeSnapshot, PressureLevel, Registry};
use ollama_router_core::http_util::{read_reqwest_capped, reqwest_error_for_log};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::http::AppState;

/// Loop until `shutdown` is cancelled. Spawn only when `policy.model_warm_enabled`.
pub async fn run(state: AppState, shutdown: CancellationToken) {
    if !state.config.policy.model_warm_enabled {
        tracing::info!("warm_keeper_disabled");
        return;
    }
    let interval = Duration::from_secs_f64(state.config.policy.model_warm_interval_seconds);
    tokio::select! {
        biased;
        () = shutdown.cancelled() => return,
        () = tokio::time::sleep(interval.min(Duration::from_secs(5))) => {}
    }
    let mut cooldowns: HashMap<NodeId, Instant> = HashMap::new();
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        tick(&state, &mut cooldowns, &shutdown).await;
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

async fn tick(
    state: &AppState,
    cooldowns: &mut HashMap<NodeId, Instant>,
    shutdown: &CancellationToken,
) {
    if state.config.effective_model_tiers().is_empty() {
        return;
    }
    let nodes: Vec<NodeSnapshot> = state
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| n.healthy && !n.draining && n.url.is_some())
        .collect();
    for node in nodes {
        if shutdown.is_cancelled() {
            return;
        }
        maybe_warm_one(state, &node, cooldowns).await;
    }
}

async fn maybe_warm_one(
    state: &AppState,
    node: &NodeSnapshot,
    cooldowns: &mut HashMap<NodeId, Instant>,
) {
    let policy = &state.config.policy;
    if node.pressure_level == PressureLevel::Critical {
        return;
    }
    let now = Instant::now();
    if let Some(last) = cooldowns.get(&node.id) {
        if now.duration_since(*last) < Duration::from_secs_f64(policy.model_warm_cooldown_seconds) {
            return;
        }
    }
    let cap = node.max_inflight_effective(policy.default_max_inflight);
    if cap > 0 && f64::from(node.inflight) / f64::from(cap) > policy.model_warm_max_inflight_ratio {
        return;
    }
    let free_vram = node.vram_free_gb.or_else(|| {
        if node.vram_gb() > 0.0 {
            Some(node.vram_gb() - node.loaded_vram_gb.unwrap_or(0.0))
        } else {
            Some(node.vram_gb())
        }
    });
    if let Some(free) = free_vram {
        if free < policy.model_warm_min_free_vram_gb {
            return;
        }
    }
    let expected = state.config.tier_models_for_vram(node.vram_gb());
    if expected.is_empty() {
        return;
    }
    let cold = expected
        .into_iter()
        .find(|model| !node.has_model_loaded(model) && node.has_model(model));
    let Some(model) = cold else {
        return;
    };
    cooldowns.insert(node.id.clone(), now);
    let timeout = Duration::from_secs_f64(policy.model_warm_cooldown_seconds * 0.8);
    match tokio::time::timeout(timeout, warm_model(state, node, &model)).await {
        Ok(()) => {}
        Err(_) => {
            tracing::debug!(node = %node.id, model = %model, "warm_request_timeout");
        }
    }
}

async fn warm_model(state: &AppState, node: &NodeSnapshot, model: &str) {
    let Some(base) = node.url.as_deref() else {
        return;
    };
    let url = format!("{}/api/generate", base.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "prompt": ".",
        "stream": false,
        "options": { "num_predict": 1 },
    });
    let Ok(payload) = serde_json::to_vec(&body) else {
        return;
    };
    let _guard = OccupancyGuard::new(state.registry.clone(), node.id.clone());
    let timeout = Duration::from_secs_f64(state.config.policy.model_warm_cooldown_seconds * 0.8);
    let resp = match state
        .client
        .post(&url)
        .timeout(timeout)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(err) => {
            tracing::debug!(
                node = %node.id,
                error = %reqwest_error_for_log(err),
                "warm_request_failed"
            );
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::debug!(
            node = %node.id,
            status = resp.status().as_u16(),
            "warm_request_http"
        );
        return;
    }
    if read_reqwest_capped(resp, state.config.health.max_probe_body_bytes)
        .await
        .is_err()
    {
        return;
    }
    tracing::info!(node = %node.id, model, "warm_model_ok");
}

struct OccupancyGuard {
    registry: Arc<Registry>,
    id: NodeId,
}

impl OccupancyGuard {
    fn new(registry: Arc<Registry>, id: NodeId) -> Self {
        registry.occupancy_inc(&id);
        Self { registry, id }
    }
}

impl Drop for OccupancyGuard {
    fn drop(&mut self) {
        self.registry.occupancy_dec(&self.id);
    }
}
