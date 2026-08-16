//! Per-node health probes: tags, soft `/api/ps`, soft capacity. Never idle.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ollama_router_core::capacity::{bytes_to_gib, capacity_target, CapacityClient};
use ollama_router_core::config::HealthConfig;
use ollama_router_core::fleet::{
    routing_url_blocked_reason, NodeId, NodeSnapshot, PressureLevel, TagRecord,
};
use ollama_router_core::http_util::{read_reqwest_capped, reqwest_error_for_log, ProbeBodyError};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::http::AppState;

const SUPERVISOR_TICK: Duration = Duration::from_millis(200);

#[derive(Default)]
struct ProbeTasks {
    tasks: HashMap<NodeId, JoinHandle<()>>,
}

impl ProbeTasks {
    fn abort_stale(&mut self, live: &HashSet<NodeId>) {
        let stale: Vec<NodeId> = self
            .tasks
            .keys()
            .filter(|id| !live.contains(id))
            .cloned()
            .collect();
        for id in stale {
            if let Some(handle) = self.tasks.remove(&id) {
                handle.abort();
            }
        }
    }

    fn abort_all(&mut self) {
        for (_, handle) in self.tasks.drain() {
            handle.abort();
        }
    }

    fn contains(&self, id: &NodeId) -> bool {
        self.tasks.contains_key(id)
    }

    fn insert(&mut self, id: NodeId, handle: JoinHandle<()>) {
        self.tasks.insert(id, handle);
    }
}

impl Drop for ProbeTasks {
    fn drop(&mut self) {
        self.abort_all();
    }
}

/// Loop until `shutdown` is cancelled. Does not count as client inflight / idle.
pub async fn run(state: AppState, shutdown: CancellationToken) {
    let n = state.registry.health().max_concurrent_probes.max(1) as usize;
    let sem = Arc::new(Semaphore::new(n));
    let mut tasks = ProbeTasks::default();
    let mut rng = seed();
    loop {
        if shutdown.is_cancelled() {
            tasks.abort_all();
            return;
        }
        reconcile_tasks(&state, &sem, &mut tasks, &mut rng, &shutdown);
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                tasks.abort_all();
                return;
            }
            () = tokio::time::sleep(SUPERVISOR_TICK) => {}
        }
    }
}

/// Run one immediate probe for every live node. This is intentionally separate
/// from the supervisor loop so an operator recheck never changes client
/// activity or the normal probe cadence.
pub async fn recheck_all(state: &AppState) {
    let health = state.registry.health().clone();
    let nodes = state
        .registry
        .snapshot()
        .into_iter()
        .filter(|node| !node.draining)
        .collect::<Vec<_>>();
    for node in nodes {
        probe_cycle(state, &health, &node, true).await;
    }
}

fn reconcile_tasks(
    state: &AppState,
    sem: &Arc<Semaphore>,
    tasks: &mut ProbeTasks,
    rng: &mut u64,
    shutdown: &CancellationToken,
) {
    state.registry.sweep_drained();
    let snap = state.registry.snapshot();
    let live: HashSet<NodeId> = snap
        .iter()
        .filter(|n| !n.draining)
        .map(|n| n.id.clone())
        .collect();

    tasks.abort_stale(&live);

    for node in snap.into_iter().filter(|n| !n.draining) {
        if tasks.contains(&node.id) {
            continue;
        }
        let state = state.clone();
        let sem = sem.clone();
        let id = node.id.clone();
        let jitter_seed = next_u64(rng);
        let shutdown = shutdown.clone();
        let handle = tokio::spawn(async move {
            probe_loop(state, sem, id, jitter_seed, shutdown).await;
        });
        tasks.insert(node.id, handle);
    }
}

async fn probe_loop(
    state: AppState,
    sem: Arc<Semaphore>,
    id: NodeId,
    mut rng: u64,
    shutdown: CancellationToken,
) {
    let mut capacity_tick: u32 = 0;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if state.registry.get(&id).is_none_or(|n| n.draining) {
            return;
        }
        if let Some(at) = state.registry.next_probe_at(&id) {
            let wait = at.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
        if shutdown.is_cancelled() {
            return;
        }
        if state.registry.get(&id).is_none_or(|n| n.draining) {
            return;
        }
        let permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            permit = sem.acquire() => permit,
        };
        let Ok(_permit) = permit else {
            return;
        };
        let health = state.registry.health().clone();
        capacity_tick = capacity_tick.saturating_add(1);
        let every = health.capacity_probe_every_n_probes.max(1);
        let do_capacity = health.capacity_probe_enabled && capacity_tick.is_multiple_of(every);
        let Some(node) = state.registry.get(&id) else {
            return;
        };
        probe_cycle(&state, &health, &node, do_capacity).await;
        schedule_next(&state, &health, &id, &mut rng);
    }
}

async fn probe_cycle(
    state: &AppState,
    health: &HealthConfig,
    node: &NodeSnapshot,
    do_capacity: bool,
) {
    let start = Instant::now();
    probe_cycle_inner(state, health, node, do_capacity).await;
    state.metrics.observe_probe(start.elapsed());
}

async fn probe_cycle_inner(
    state: &AppState,
    health: &HealthConfig,
    node: &NodeSnapshot,
    do_capacity: bool,
) {
    match node.url.as_deref() {
        None => {
            state.registry.mark_unreachable(&node.id, "no_url");
            return;
        }
        Some(url)
            if routing_url_blocked_reason(url, state.registry.public_share_suffixes())
                .is_some() =>
        {
            state.metrics.route_reason("public_url_blocked");
            state
                .registry
                .mark_unreachable(&node.id, "public_url_blocked");
            return;
        }
        Some(_) => {}
    }

    let Some(base) = node.url.as_deref() else {
        return;
    };
    let tags_url = format!("{}/api/tags", base.trim_end_matches('/'));
    let probe_timeout = Duration::from_secs_f64(health.probe_timeout_seconds);
    let request = state.client.get(&tags_url).timeout(probe_timeout).send();
    match request.await {
        Ok(resp) if resp.status().is_success() => {
            match parse_tags(resp, health.max_probe_body_bytes).await {
                Ok(records) => {
                    state.registry.update_models_from_records(&node.id, records);
                    state.registry.note_probe_success(&node.id);
                    if health.ps_probe_enabled {
                        probe_ps(state, health, node).await;
                    }
                    if do_capacity {
                        probe_capacity(state, health, node).await;
                    }
                }
                Err(_) => {
                    tracing::debug!(node = %node.id, "health probe tags body rejected");
                    state
                        .registry
                        .note_probe_failure(&node.id, "probe_failures");
                }
            }
        }
        Ok(resp) => {
            tracing::debug!(
                node = %node.id,
                status = resp.status().as_u16(),
                "health probe failed"
            );
            state
                .registry
                .note_probe_failure(&node.id, "probe_failures");
        }
        Err(err) => {
            tracing::debug!(
                node = %node.id,
                error = %reqwest_error_for_log(err),
                "health probe error"
            );
            state
                .registry
                .note_probe_failure(&node.id, "probe_failures");
        }
    }
}

async fn probe_ps(state: &AppState, health: &HealthConfig, node: &NodeSnapshot) {
    let Some(base) = node.url.as_deref() else {
        return;
    };
    let url = format!("{}/api/ps", base.trim_end_matches('/'));
    let timeout = Duration::from_secs_f64(health.probe_timeout_seconds);
    let resp = match state.client.get(&url).timeout(timeout).send().await {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(_) | Err(_) => return,
    };
    let Ok(bytes) = read_reqwest_capped(resp, health.max_probe_body_bytes).await else {
        return;
    };
    let Ok(body) = serde_json::from_slice::<PsResponse>(&bytes) else {
        return;
    };
    let models = body.models.unwrap_or_default();
    let names: Vec<String> = models
        .iter()
        .filter_map(|m| m.name.as_deref())
        .filter(|n| !n.trim().is_empty())
        .map(str::to_string)
        .collect();
    let mut vram_bytes: u64 = 0;
    let mut any_vram = false;
    for entry in &models {
        if let Some(size) = entry.size_vram {
            vram_bytes = vram_bytes.saturating_add(size);
            any_vram = true;
        } else if let Some(size) = entry.size {
            vram_bytes = vram_bytes.saturating_add(size);
            any_vram = true;
        }
    }
    let loaded_vram = any_vram.then_some(bytes_to_gib(vram_bytes));
    state.registry.update_ps_state(&node.id, names, loaded_vram);
}

async fn probe_capacity(state: &AppState, health: &HealthConfig, node: &NodeSnapshot) {
    let pressure_path = health
        .pressure_probe_path
        .as_deref()
        .unwrap_or("/v1/pressure");
    let Some(target) = capacity_target(
        node.url.as_deref(),
        node.capacity_url.as_deref(),
        health.capacity_probe_port,
        &health.capacity_probe_path,
        pressure_path,
    ) else {
        return;
    };
    let client = CapacityClient::new(state.client.clone());
    let timeout = Duration::from_secs_f64(health.capacity_probe_timeout_seconds);
    match client
        .probe(
            &target,
            health.capacity_probe_token.as_deref(),
            timeout,
            health.max_probe_body_bytes,
        )
        .await
    {
        Ok(probe) => {
            let level = probe
                .pressure_level
                .as_deref()
                .and_then(PressureLevel::from_wire);
            state
                .registry
                .apply_capacity_report(&node.id, &probe.report, level);
        }
        Err(err) => {
            state.registry.set_capacity_error(&node.id, err.as_reason());
        }
    }
}

fn schedule_next(state: &AppState, health: &HealthConfig, id: &NodeId, rng: &mut u64) {
    let Some(node) = state.registry.get(id) else {
        return;
    };
    let base = if node.healthy {
        health.interval_seconds
    } else {
        let backoff = state.registry.probe_backoff(id);
        if backoff > 0.0 {
            backoff
        } else {
            health.interval_seconds
        }
    };
    let delay = jittered_interval(base, health.probe_jitter_ratio, rng);
    state
        .registry
        .set_next_probe_at(id, Instant::now() + Duration::from_secs_f64(delay));
}

async fn parse_tags(
    resp: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<(String, TagRecord)>, ProbeBodyError> {
    let bytes = read_reqwest_capped(resp, max_bytes).await?;
    let body = serde_json::from_slice::<TagsResponse>(&bytes).map_err(|_| ProbeBodyError::Parse)?;
    Ok(body
        .models
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let name = m
                .name
                .or(m.model)
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())?;
            Some((
                name,
                TagRecord {
                    digest: m.digest.unwrap_or_default(),
                    size: m.size,
                    modified_at: m.modified_at.filter(|s| !s.trim().is_empty()),
                    details: m.details,
                    capabilities: m.capabilities,
                },
            ))
        })
        .collect())
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Option<Vec<TagModel>>,
}

#[derive(Deserialize)]
struct TagModel {
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    details: Option<serde_json::Value>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Option<Vec<PsModel>>,
}

#[derive(Deserialize)]
struct PsModel {
    name: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    size_vram: Option<u64>,
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

/// Reload fleet.yaml into the live registry (SIGHUP). Keeps inflight streams.
pub async fn reload_permanent_inventory(state: &AppState) -> anyhow::Result<()> {
    let fleet_path = state.config.fleet_path.clone();
    let missing_is_error = state.config.fleet_missing_is_error;
    let fleet_state = Arc::clone(&state.fleet_state);
    let nodes = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let mut nodes = ollama_router_core::load_fleet_nodes(&fleet_path, missing_is_error)?;
        ollama_router_core::hydrate_node_urls(&mut nodes, fleet_state.as_ref())?;
        Ok(nodes)
    })
    .await
    .map_err(|err| anyhow::anyhow!("fleet reload blocking join: {err}"))??;
    state.registry.apply_permanent_inventory(&nodes);
    tracing::info!(
        nodes = nodes.len(),
        path = %state.config.fleet_path.display(),
        "fleet inventory reloaded"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_json_keeps_cli_fields_and_ignores_unknown() {
        let body: TagsResponse = serde_json::from_str(
            r#"{"models":[{"name":"llama3.2:1b","digest":"aaaaaaaaaaaa","size":1,"modified_at":"2026-08-01T00:00:00Z","details":{"family":"llama"},"capabilities":["completion"],"parameter_size":"1B"}]}"#,
        )
        .expect("tags json");
        let model = &body.models.expect("models")[0];
        assert_eq!(model.name.as_deref(), Some("llama3.2:1b"));
        assert_eq!(model.digest.as_deref(), Some("aaaaaaaaaaaa"));
        assert_eq!(model.size, Some(1));
        assert_eq!(model.modified_at.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(model.details.as_ref().unwrap()["family"], "llama");
        assert_eq!(model.capabilities, Some(vec!["completion".into()]));
    }
}
