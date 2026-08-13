//! Concurrent `/api/tags` health probes with jitter. Capacity is soft-fail.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ollama_router_core::config::HealthConfig;
use ollama_router_core::fleet::{NodeSnapshot, PressureLevel};
use serde::Deserialize;
use tokio::sync::Semaphore;

use crate::http::AppState;

/// Loop until the process exits. Does not count as client inflight / idle.
pub async fn run(state: AppState) {
    let mut rng = seed();
    let mut tick: u64 = 0;
    loop {
        let health = state.registry.health().clone();
        let wait = jittered_interval(health.interval_seconds, health.probe_jitter_ratio, &mut rng);
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        tick = tick.saturating_add(1);
        probe_round(&state, &health, tick, &mut rng).await;
    }
}

async fn probe_round(state: &AppState, health: &HealthConfig, tick: u64, rng: &mut u64) {
    let mut nodes: Vec<NodeSnapshot> = state
        .registry
        .snapshot()
        .into_iter()
        .filter(|n| !n.draining && n.url.is_some())
        .collect();
    shuffle(&mut nodes, rng);
    let n = health.max_concurrent_probes.max(1) as usize;
    let sem = Arc::new(Semaphore::new(n));
    let every = u64::from(health.capacity_probe_every_n_probes.max(1));
    let do_capacity = health.capacity_probe_enabled && tick.is_multiple_of(every);
    let mut set = tokio::task::JoinSet::new();
    for node in nodes {
        let permit = sem.clone();
        let state = state.clone();
        let health = health.clone();
        set.spawn(async move {
            let Ok(_permit) = permit.acquire_owned().await else {
                return;
            };
            probe_node(&state, &health, node, do_capacity).await;
        });
    }
    while set.join_next().await.is_some() {}
}

async fn probe_node(
    state: &AppState,
    health: &HealthConfig,
    node: NodeSnapshot,
    do_capacity: bool,
) {
    let Some(base) = node.url.as_deref() else {
        return;
    };
    let tags_url = format!("{}/api/tags", base.trim_end_matches('/'));
    let probe_timeout = Duration::from_secs_f64(health.probe_timeout_seconds);
    let request = state.client.get(&tags_url).timeout(probe_timeout).send();
    match request.await {
        Ok(resp) if resp.status().is_success() => {
            let names = parse_tag_names(resp).await;
            state.registry.set_healthy(&node.id);
            state.registry.update_models(&node.id, names);
            if do_capacity {
                probe_capacity(state, health, &node).await;
            }
        }
        Ok(resp) => {
            tracing::debug!(
                node = %node.id,
                status = resp.status().as_u16(),
                "health probe failed"
            );
            state.registry.mark_request_failure(&node.id);
        }
        Err(err) => {
            tracing::debug!(node = %node.id, error = %err, "health probe error");
            state.registry.mark_request_failure(&node.id);
        }
    }
}

async fn parse_tag_names(resp: reqwest::Response) -> Vec<String> {
    let Ok(bytes) = resp.bytes().await else {
        return Vec::new();
    };
    let Ok(body) = serde_json::from_slice::<TagsResponse>(&bytes) else {
        return Vec::new();
    };
    body.models
        .into_iter()
        .flatten()
        .filter_map(|m| m.name)
        .filter(|n| !n.trim().is_empty())
        .collect()
}

async fn probe_capacity(state: &AppState, health: &HealthConfig, node: &NodeSnapshot) {
    let Some(url) = capacity_url(node, health) else {
        return;
    };
    let timeout = Duration::from_secs_f64(health.capacity_probe_timeout_seconds);
    let mut req = state.client.get(&url).timeout(timeout);
    if let Some(token) = &health.capacity_probe_token {
        req = req.bearer_auth(token);
    }
    let resp = match req.send().await {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(_) | Err(_) => return,
    };
    let Ok(bytes) = resp.bytes().await else {
        return;
    };
    let Ok(body) = serde_json::from_slice::<CapacityHint>(&bytes) else {
        return;
    };
    if let Some(raw) = body.pressure_level.as_deref() {
        if let Some(level) = PressureLevel::from_wire(raw) {
            state.registry.set_pressure_level(&node.id, level);
        }
    }
}

fn capacity_url(node: &NodeSnapshot, health: &HealthConfig) -> Option<String> {
    if let Some(url) = node.capacity_url.as_deref() {
        return Some(url.to_string());
    }
    let ollama = node.url.as_deref()?;
    let mut parsed = url::Url::parse(ollama).ok()?;
    let _ = parsed.set_port(Some(health.capacity_probe_port));
    parsed.set_path(&health.capacity_probe_path);
    parsed.set_query(None);
    Some(parsed.to_string())
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Option<Vec<TagModel>>,
}

#[derive(Deserialize)]
struct TagModel {
    name: Option<String>,
}

#[derive(Deserialize)]
struct CapacityHint {
    pressure_level: Option<String>,
}

fn seed() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    now ^ u64::from(std::process::id())
}

fn next_u64(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state
}

fn jittered_interval(interval: f64, jitter_ratio: f64, rng: &mut u64) -> f64 {
    let ratio = jitter_ratio.clamp(0.0, 1.0);
    if interval <= 0.0 {
        return 1.0;
    }
    if ratio == 0.0 {
        return interval;
    }
    let span = interval * ratio;
    let unit = (next_u64(rng) >> 11) as f64 / ((1u64 << 53) as f64);
    (interval - span + 2.0 * span * unit).max(0.05)
}

fn shuffle<T>(items: &mut [T], rng: &mut u64) {
    for i in (1..items.len()).rev() {
        let j = (next_u64(rng) as usize) % (i + 1);
        items.swap(i, j);
    }
}

/// Reload fleet.yaml into the live registry (SIGHUP). Keeps inflight streams.
pub fn reload_permanent_inventory(state: &AppState) -> anyhow::Result<()> {
    let mut nodes = ollama_router_core::load_fleet_nodes(
        &state.config.fleet_path,
        state.config.fleet_missing_is_error,
    )?;
    let fleet_state = ollama_router_core::FleetState::new(&state.config.state_path);
    ollama_router_core::hydrate_node_urls(&mut nodes, &fleet_state)?;
    state.registry.apply_permanent_inventory(&nodes);
    tracing::info!(
        nodes = nodes.len(),
        path = %state.config.fleet_path.display(),
        "fleet inventory reloaded"
    );
    Ok(())
}
