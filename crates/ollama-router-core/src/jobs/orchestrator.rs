//! Placement-aware pull/delete with SQLite restart recovery.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::{watch, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::config::RouterConfig;
use crate::fleet::ids::NodeId;
use crate::fleet::registry::{normalize_model, NodeSnapshot, Registry};
use crate::routing::{
    placement_eligible_node_ids, ram_pressure_blocks_placement, resolve_target_nodes, TargetSpec,
};

use super::store::{unix_now, JobStore};
use super::types::{Job, JobId, JobKind, JobStatus, JobTarget, TargetStatus};
use super::{JobFuture, JobObserver, JobOutcome, ModelOrchestrator, OrchestratorError};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DedupeKey {
    kind: JobKind,
    models: Vec<String>,
    pairs: Vec<(String, String)>,
}

struct State {
    jobs: HashMap<JobId, Job>,
    inflight: HashMap<DedupeKey, JobId>,
    waiters: HashMap<JobId, watch::Sender<Job>>,
    recovery_ids: HashSet<JobId>,
    tasks: HashMap<JobId, JoinHandle<()>>,
    node_slots: HashMap<String, Arc<Semaphore>>,
}

struct Inner {
    config: Arc<RouterConfig>,
    client: reqwest::Client,
    registry: Option<Arc<Registry>>,
    store: Option<JobStore>,
    state: Mutex<State>,
    observer: Mutex<Option<Arc<dyn JobObserver>>>,
    shutdown: CancellationToken,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, handle) in state.tasks.drain() {
            handle.abort();
        }
    }
}

/// Fleet pull/delete orchestrator. Cheap to clone (`Arc`).
#[derive(Clone)]
pub struct PullOrchestrator {
    inner: Arc<Inner>,
}

impl PullOrchestrator {
    /// Open the store (when configured), rebuild the in-flight map, prune.
    pub fn new(
        config: Arc<RouterConfig>,
        client: reqwest::Client,
        registry: Option<Arc<Registry>>,
    ) -> Result<Self, OrchestratorError> {
        Self::with_shutdown(config, client, registry, CancellationToken::new())
    }

    /// Same as [`Self::new`], sharing the process shutdown token.
    pub fn with_shutdown(
        config: Arc<RouterConfig>,
        client: reqwest::Client,
        registry: Option<Arc<Registry>>,
        shutdown: CancellationToken,
    ) -> Result<Self, OrchestratorError> {
        let store = match config.job_store_path.as_deref() {
            Some(path) if !path.trim().is_empty() => Some(JobStore::open(path)?),
            _ => None,
        };
        let loaded = store
            .as_ref()
            .map(JobStore::load)
            .transpose()
            .map_err(OrchestratorError::from)?;
        let mut jobs = HashMap::new();
        let mut inflight = HashMap::new();
        let mut waiters = HashMap::new();
        let mut recovery_ids = HashSet::new();
        if let Some(loaded) = loaded {
            for (id, job) in loaded {
                if job.status == JobStatus::Running {
                    let key = dedupe_key(job.kind, &job.models, &pairs_from_job(&job));
                    inflight.insert(key, id);
                    recovery_ids.insert(id);
                    let (tx, _) = watch::channel(job.clone());
                    waiters.insert(id, tx);
                }
                jobs.insert(id, job);
            }
        }
        let orch = Self {
            inner: Arc::new(Inner {
                config,
                client,
                registry,
                store,
                state: Mutex::new(State {
                    jobs,
                    inflight,
                    waiters,
                    recovery_ids,
                    tasks: HashMap::new(),
                    node_slots: HashMap::new(),
                }),
                observer: Mutex::new(None),
                shutdown,
            }),
        };
        orch.prune_blocking(0);
        Ok(orch)
    }

    /// Resume operations persisted as `running` (tags-first).
    pub async fn recover_incomplete_jobs(&self) -> Vec<Job> {
        let ids: Vec<JobId> = {
            let state = self.lock();
            state.recovery_ids.iter().copied().collect()
        };
        self.start_recovery();
        let mut recovered = Vec::new();
        for id in ids {
            recovered.push(self.wait_job(&id).await);
        }
        recovered
    }

    /// Snapshot a job (admin GET).
    pub fn get_job(&self, id: &JobId) -> Option<Job> {
        self.lock().jobs.get(id).cloned()
    }

    /// In-memory jobs, newest first (admin GET /jobs).
    pub fn list_jobs(&self) -> Vec<Job> {
        let mut jobs: Vec<Job> = self.lock().jobs.values().cloned().collect();
        jobs.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.id.as_uuid().cmp(&a.id.as_uuid()))
        });
        jobs
    }

    /// Attach a terminal-status observer (binary metrics). Replaces any previous hook.
    pub fn set_observer(&self, observer: Arc<dyn JobObserver>) {
        *self
            .inner
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer);
    }

    /// Register + spawn a pull. Duplicate in-flight work coalesces.
    pub async fn start_ensure(
        &self,
        models: &[String],
        spec: TargetSpec,
        include_unhealthy: bool,
        avoid_ram_pressure: bool,
    ) -> Result<Job, OrchestratorError> {
        let models = normalize_models(models)?;
        let targets = self.targets_for(&models, &spec, include_unhealthy, avoid_ram_pressure)?;
        if pairs_of(&targets).is_empty() {
            return Err(OrchestratorError::NoPlacementTargets);
        }
        let ineligible = self.capacity_ineligible(&models, &targets);
        let ram_blocked = self.ram_pressure_ineligible(&models, &targets, avoid_ram_pressure);
        self.register_and_spawn(JobKind::Pull, models, targets, ineligible, ram_blocked)
            .await
    }

    /// Ensure with an already-resolved per-model node map (CLI tier fan-out).
    pub async fn start_ensure_targets(
        &self,
        targets: BTreeMap<String, Vec<NodeId>>,
    ) -> Result<Job, OrchestratorError> {
        if targets.is_empty() || pairs_of(&targets).is_empty() {
            return Err(OrchestratorError::NoPlacementTargets);
        }
        let models: Vec<String> = targets.keys().cloned().collect();
        let models = normalize_models(&models)?;
        let ineligible = self.capacity_ineligible(&models, &targets);
        let ram_blocked = self.ram_pressure_ineligible(&models, &targets, false);
        self.register_and_spawn(JobKind::Pull, models, targets, ineligible, ram_blocked)
            .await
    }

    /// Register + spawn a delete.
    pub async fn start_delete(
        &self,
        models: &[String],
        spec: TargetSpec,
        include_unhealthy: bool,
    ) -> Result<Job, OrchestratorError> {
        let models = normalize_models(models)?;
        let targets = self.delete_targets(&models, &spec, include_unhealthy)?;
        if pairs_of(&targets).is_empty() {
            return Err(OrchestratorError::NoTargetNodes);
        }
        self.register_and_spawn(
            JobKind::Delete,
            models,
            targets,
            HashSet::new(),
            HashSet::new(),
        )
        .await
    }

    /// Block until the job is terminal.
    pub async fn wait_job(&self, id: &JobId) -> Job {
        let mut rx = {
            let state = self.lock();
            if let Some(tx) = state.waiters.get(id) {
                tx.subscribe()
            } else if let Some(job) = state.jobs.get(id) {
                return job.clone();
            } else {
                return Job {
                    id: *id,
                    kind: JobKind::Pull,
                    status: JobStatus::Failed,
                    created_at: unix_now(),
                    finished_at: Some(unix_now()),
                    models: Vec::new(),
                    nodes: Vec::new(),
                    targets: BTreeMap::new(),
                };
            }
        };
        loop {
            let job = rx.borrow().clone();
            if !job.status.is_incomplete() {
                return job;
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone();
            }
        }
    }

    /// Await with a ceiling (admin `wait`).
    pub async fn wait_job_timeout(&self, id: &JobId, timeout: Duration) -> (Job, bool) {
        match tokio::time::timeout(timeout, self.wait_job(id)).await {
            Ok(job) => (job, false),
            Err(_) => (
                self.get_job(id).unwrap_or_else(|| Job {
                    id: *id,
                    kind: JobKind::Pull,
                    status: JobStatus::Running,
                    created_at: unix_now(),
                    finished_at: None,
                    models: Vec::new(),
                    nodes: Vec::new(),
                    targets: BTreeMap::new(),
                }),
                true,
            ),
        }
    }
}

impl ModelOrchestrator for PullOrchestrator {
    fn ensure(&self, model: &str) -> JobFuture<'_> {
        let model = model.to_string();
        Box::pin(async move {
            let job = self
                .start_ensure(&[model], TargetSpec::Placement, false, false)
                .await?;
            Ok(outcome(&self.wait_job(&job.id).await))
        })
    }

    fn delete(&self, model: &str) -> JobFuture<'_> {
        let model = model.to_string();
        Box::pin(async move {
            let job = self
                .start_delete(&[model], TargetSpec::Placement, false)
                .await?;
            Ok(outcome(&self.wait_job(&job.id).await))
        })
    }
}

impl PullOrchestrator {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn snapshots(&self) -> Vec<NodeSnapshot> {
        if let Some(reg) = &self.inner.registry {
            return reg.snapshot();
        }
        let tmp = Registry::new(&self.inner.config);
        for node in tmp.snapshot() {
            tmp.set_healthy(&node.id);
        }
        tmp.snapshot()
    }

    fn targets_for(
        &self,
        models: &[String],
        spec: &TargetSpec,
        include_unhealthy: bool,
        avoid_ram_pressure: bool,
    ) -> Result<BTreeMap<String, Vec<NodeId>>, OrchestratorError> {
        let nodes = self.snapshots();
        resolve_target_nodes(
            &nodes,
            models,
            spec,
            &self.inner.config.policy,
            include_unhealthy,
            avoid_ram_pressure,
        )
        .map_err(OrchestratorError::from)
    }

    fn delete_targets(
        &self,
        models: &[String],
        spec: &TargetSpec,
        include_unhealthy: bool,
    ) -> Result<BTreeMap<String, Vec<NodeId>>, OrchestratorError> {
        if matches!(spec, TargetSpec::Placement) {
            if let Some(reg) = &self.inner.registry {
                let snap = reg.snapshot();
                let mut map = BTreeMap::new();
                for model in models {
                    let holders: Vec<NodeId> = snap
                        .iter()
                        .filter(|n| n.healthy && n.has_model(model))
                        .map(|n| n.id.clone())
                        .collect();
                    map.insert(model.clone(), holders);
                }
                return Ok(map);
            }
        }
        self.targets_for(models, spec, include_unhealthy, false)
    }

    fn capacity_ineligible(
        &self,
        models: &[String],
        targets: &BTreeMap<String, Vec<NodeId>>,
    ) -> HashSet<(String, String)> {
        let nodes = self.snapshots();
        let policy = &self.inner.config.policy;
        let mut ineligible = HashSet::new();
        for model in models {
            let eligible: HashSet<String> =
                placement_eligible_node_ids(&nodes, model, policy, true, false)
                    .into_iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
            for node_id in targets.get(model).into_iter().flatten() {
                if !eligible.contains(node_id.as_str()) {
                    ineligible.insert((node_id.as_str().to_string(), model.clone()));
                }
            }
        }
        ineligible
    }

    fn ram_pressure_ineligible(
        &self,
        models: &[String],
        targets: &BTreeMap<String, Vec<NodeId>>,
        strict: bool,
    ) -> HashSet<(String, String)> {
        let Some(reg) = &self.inner.registry else {
            return HashSet::new();
        };
        let policy = &self.inner.config.policy;
        let mut blocked = HashSet::new();
        for model in models {
            let class = crate::routing::placement_class(model, policy);
            for node_id in targets.get(model).into_iter().flatten() {
                if let Some(node) = reg.get(node_id) {
                    if ram_pressure_blocks_placement(&node, class, policy, strict) {
                        blocked.insert((node_id.as_str().to_string(), model.clone()));
                    }
                }
            }
        }
        blocked
    }

    fn start_recovery(&self) {
        let ids: Vec<JobId> = {
            let state = self.lock();
            state
                .recovery_ids
                .iter()
                .copied()
                .filter(|id| {
                    state
                        .jobs
                        .get(id)
                        .is_some_and(|j| j.status == JobStatus::Running)
                        && !state.tasks.contains_key(id)
                })
                .collect()
        };
        for id in ids {
            let job = match self.get_job(&id) {
                Some(job) => job,
                None => continue,
            };
            let models = job.models.clone();
            let targets = node_id_targets_of(&job);
            let ineligible = if job.kind == JobKind::Pull {
                self.capacity_ineligible(&models, &targets)
            } else {
                HashSet::new()
            };
            self.spawn_run(id, ineligible, HashSet::new());
        }
    }

    async fn register_and_spawn(
        &self,
        kind: JobKind,
        models: Vec<String>,
        targets: BTreeMap<String, Vec<NodeId>>,
        ineligible: HashSet<(String, String)>,
        ram_blocked: HashSet<(String, String)>,
    ) -> Result<Job, OrchestratorError> {
        self.start_recovery();
        let key = dedupe_key(kind, &models, &pairs_from_map(&targets));
        {
            let state = self.lock();
            if let Some(existing_id) = state.inflight.get(&key).copied() {
                if let Some(existing) = state.jobs.get(&existing_id) {
                    if existing.status == JobStatus::Running {
                        return Ok(existing.clone());
                    }
                }
            }
        }
        self.prune(1).await;
        let job = new_job(kind, models, &targets);
        let id = job.id;
        {
            let mut state = self.lock();
            if let Some(existing_id) = state.inflight.get(&key).copied() {
                if let Some(existing) = state.jobs.get(&existing_id) {
                    if existing.status == JobStatus::Running {
                        return Ok(existing.clone());
                    }
                }
            }
            let (tx, _) = watch::channel(job.clone());
            state.waiters.insert(id, tx);
            state.jobs.insert(id, job.clone());
            state.inflight.insert(key, id);
        }
        self.persist(&job).await;
        self.spawn_run(id, ineligible, ram_blocked);
        Ok(job)
    }

    fn spawn_run(
        &self,
        id: JobId,
        ineligible: HashSet<(String, String)>,
        ram_blocked: HashSet<(String, String)>,
    ) {
        let orch = self.clone();
        let mut state = self.lock();
        let handle = tokio::spawn(async move {
            orch.run_job(id, ineligible, ram_blocked).await;
        });
        state.tasks.insert(id, handle);
    }

    #[cfg(test)]
    pub(crate) fn job_task_count(&self) -> usize {
        self.lock().tasks.len()
    }

    async fn run_job(
        &self,
        id: JobId,
        ineligible: HashSet<(String, String)>,
        ram_blocked: HashSet<(String, String)>,
    ) {
        let job = match self.get_job(&id) {
            Some(job) => job,
            None => {
                self.lock().tasks.remove(&id);
                return;
            }
        };
        let pairs: Vec<(String, String)> = job
            .targets
            .values()
            .map(|t| (t.node.clone(), t.model.clone()))
            .collect();
        let kind = job.kind;

        for (node_id, model) in &pairs {
            if ineligible.contains(&(node_id.clone(), model.clone()))
                && target_incomplete(&job, node_id, model)
            {
                self.set_target(
                    &id,
                    node_id,
                    model,
                    TargetStatus::SkippedCapacity,
                    Some("node capacity below the model class requirement".into()),
                )
                .await;
            }
        }
        let job = self.get_job(&id).unwrap_or(job);
        for (node_id, model) in &pairs {
            if ram_blocked.contains(&(node_id.clone(), model.clone()))
                && !ineligible.contains(&(node_id.clone(), model.clone()))
                && target_incomplete(&job, node_id, model)
            {
                self.set_target(
                    &id,
                    node_id,
                    model,
                    TargetStatus::SkippedRamPressure,
                    Some("node RAM pressure exceeds the placement policy".into()),
                )
                .await;
            }
        }

        let mut set = JoinSet::new();
        let mut keys: HashMap<tokio::task::Id, (String, String)> = HashMap::new();
        let job = self.get_job(&id).unwrap_or_else(|| job.clone());
        for (node_id, model) in pairs {
            if ineligible.contains(&(node_id.clone(), model.clone()))
                || ram_blocked.contains(&(node_id.clone(), model.clone()))
            {
                continue;
            }
            if !target_incomplete(&job, &node_id, &model) {
                continue;
            }
            if self.inner.shutdown.is_cancelled() {
                self.set_target(
                    &id,
                    &node_id,
                    &model,
                    TargetStatus::Failed,
                    Some("shutdown".into()),
                )
                .await;
                continue;
            }
            let orch = self.clone();
            let n = node_id.clone();
            let m = model.clone();
            let abort = set.spawn(async move {
                match kind {
                    JobKind::Pull => orch.ensure_one(id, n, m).await,
                    JobKind::Delete => orch.delete_one(id, n, m).await,
                    JobKind::Provision => {
                        orch.set_target(
                            &id,
                            &n,
                            &m,
                            TargetStatus::Failed,
                            Some("provision jobs are managed by the cloud manager".into()),
                        )
                        .await
                    }
                }
            });
            keys.insert(abort.id(), (node_id, model));
        }
        while let Some(joined) = set.join_next_with_id().await {
            match joined {
                Ok((tid, ())) => {
                    keys.remove(&tid);
                }
                Err(err) => {
                    if let Some((node, model)) = keys.remove(&err.id()) {
                        tracing::warn!(
                            job_id = %id,
                            node = %node,
                            model = %model,
                            error = %err,
                            "job target join failed"
                        );
                    } else {
                        tracing::warn!(job_id = %id, error = %err, "job target join failed");
                    }
                }
            }
        }

        let snap = {
            let mut state = self.lock();
            let snap = state.jobs.get_mut(&id).map(|job| {
                job.status = job.summarize_status();
                job.finished_at = Some(unix_now());
                job.clone()
            });
            let key = snap
                .as_ref()
                .map(|j| dedupe_key(j.kind, &j.models, &pairs_from_job(j)));
            if let Some(key) = key {
                if state.inflight.get(&key) == Some(&id) {
                    state.inflight.remove(&key);
                }
            }
            state.tasks.remove(&id);
            state.recovery_ids.remove(&id);
            snap
        };
        if let Some(snap) = snap {
            self.persist(&snap).await;
            self.notify(&snap);
            self.observe_terminal(&snap);
        }
    }

    async fn ensure_one(&self, job_id: JobId, node_id: String, model: String) {
        if self.node_unhealthy(&node_id) {
            self.set_target(
                &job_id,
                &node_id,
                &model,
                TargetStatus::SkippedUnhealthy,
                Some("node unhealthy at dispatch time".into()),
            )
            .await;
            return;
        }
        let (url, err) = self.resolve_url(&node_id);
        if let Some(err) = err {
            self.set_target(&job_id, &node_id, &model, TargetStatus::Failed, Some(err))
                .await;
            return;
        }
        let Some(url) = url else {
            self.set_target(
                &job_id,
                &node_id,
                &model,
                TargetStatus::Failed,
                Some("unknown node".into()),
            )
            .await;
            return;
        };

        let present = match self.probe_tags(&url).await {
            Ok(set) => set,
            Err(detail) => {
                self.set_target(
                    &job_id,
                    &node_id,
                    &model,
                    TargetStatus::Failed,
                    Some(detail),
                )
                .await;
                return;
            }
        };
        let normalized = normalize_model(&model);
        if present.contains(&normalized) {
            self.set_target(
                &job_id,
                &node_id,
                &model,
                TargetStatus::AlreadyPresent,
                None,
            )
            .await;
            self.refresh_registry(&node_id, present);
            return;
        }

        let slot = self.slot_for(&node_id);
        let _permit = match slot.acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                self.set_target(
                    &job_id,
                    &node_id,
                    &model,
                    TargetStatus::Failed,
                    Some("pull slot closed".into()),
                )
                .await;
                return;
            }
        };
        self.set_target(&job_id, &node_id, &model, TargetStatus::Running, None)
            .await;
        tracing::info!(node = %node_id, model = %model, "pull_start");
        let (ok, detail) = self.pull(&url, &model).await;
        self.set_target(
            &job_id,
            &node_id,
            &model,
            if ok {
                TargetStatus::Success
            } else {
                TargetStatus::Failed
            },
            detail,
        )
        .await;
        tracing::info!(node = %node_id, model = %model, ok, "pull_finish");
        if ok {
            let mut next = present;
            next.insert(normalized);
            self.refresh_registry(&node_id, next);
        }
    }

    async fn delete_one(&self, job_id: JobId, node_id: String, model: String) {
        if self.node_unhealthy(&node_id) {
            self.set_target(
                &job_id,
                &node_id,
                &model,
                TargetStatus::SkippedUnhealthy,
                Some("node unhealthy at dispatch time".into()),
            )
            .await;
            return;
        }
        let (url, err) = self.resolve_url(&node_id);
        if let Some(err) = err {
            self.set_target(&job_id, &node_id, &model, TargetStatus::Failed, Some(err))
                .await;
            return;
        }
        let Some(url) = url else {
            self.set_target(
                &job_id,
                &node_id,
                &model,
                TargetStatus::Failed,
                Some("unknown node".into()),
            )
            .await;
            return;
        };
        let present = match self.probe_tags(&url).await {
            Ok(set) => set,
            Err(detail) => {
                self.set_target(
                    &job_id,
                    &node_id,
                    &model,
                    TargetStatus::Failed,
                    Some(detail),
                )
                .await;
                return;
            }
        };
        let normalized = normalize_model(&model);
        if !present.contains(&normalized) {
            self.set_target(&job_id, &node_id, &model, TargetStatus::AlreadyAbsent, None)
                .await;
            self.refresh_registry(&node_id, present);
            return;
        }
        tracing::info!(node = %node_id, model = %model, "delete_start");
        let (ok, detail) = self.delete_model(&url, &model).await;
        if ok {
            self.set_target(&job_id, &node_id, &model, TargetStatus::Deleted, detail)
                .await;
            tracing::info!(node = %node_id, model = %model, ok = true, "delete_finish");
            let mut next = present;
            next.remove(&normalized);
            self.refresh_registry(&node_id, next);
            return;
        }
        self.set_target(&job_id, &node_id, &model, TargetStatus::Failed, detail)
            .await;
        tracing::info!(node = %node_id, model = %model, ok = false, "delete_finish");
    }

    fn node_unhealthy(&self, node_id: &str) -> bool {
        let Some(reg) = &self.inner.registry else {
            return false;
        };
        let Ok(id) = NodeId::parse(node_id) else {
            return false;
        };
        reg.get(&id).is_some_and(|n| !n.healthy)
    }

    fn resolve_url(&self, node_id: &str) -> (Option<String>, Option<String>) {
        if let Some(reg) = &self.inner.registry {
            if let Ok(id) = NodeId::parse(node_id) {
                if let Some(node) = reg.get(&id) {
                    return match node.url.as_deref() {
                        Some(url) if !url.is_empty() => (Some(url.to_string()), None),
                        _ => (None, Some("node has no url (not yet provisioned)".into())),
                    };
                }
            }
        }
        match self
            .inner
            .config
            .nodes
            .iter()
            .find(|n| n.id.as_str() == node_id)
        {
            None => (None, Some("unknown node".into())),
            Some(node) => match &node.url {
                Some(url) if !url.is_empty() => (Some(url.clone()), None),
                _ => (None, Some("node has no url (not yet provisioned)".into())),
            },
        }
    }

    fn slot_for(&self, node_id: &str) -> Arc<Semaphore> {
        let mut state = self.lock();
        state
            .node_slots
            .entry(node_id.to_string())
            .or_insert_with(|| {
                Arc::new(Semaphore::new(
                    self.inner.config.max_pulls_per_node.max(1) as usize
                ))
            })
            .clone()
    }

    fn refresh_registry(&self, node_id: &str, models: HashSet<String>) {
        let Some(reg) = &self.inner.registry else {
            return;
        };
        let Ok(id) = NodeId::parse(node_id) else {
            return;
        };
        let mut list: Vec<String> = models.into_iter().collect();
        list.sort();
        reg.update_models(&id, list);
    }

    async fn probe_tags(&self, base_url: &str) -> Result<HashSet<String>, String> {
        let timeout = Duration::from_secs_f64(self.inner.config.timeouts.connect_seconds + 5.0);
        let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
        let resp = self
            .inner
            .client
            .get(&url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| format!("tags probe failed: {}", truncate(&err.to_string())))?;
        if !resp.status().is_success() {
            return Err(format!(
                "tags probe failed: http {}",
                resp.status().as_u16()
            ));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|err| format!("tags probe failed: {}", truncate(&err.to_string())))?;
        let parsed: TagsBody =
            serde_json::from_slice(&bytes).map_err(|_| "tags probe failed: parse".to_string())?;
        Ok(parsed
            .models
            .into_iter()
            .map(|m| normalize_model(&m.name))
            .filter(|n| !n.is_empty())
            .collect())
    }

    async fn pull(&self, base_url: &str, model: &str) -> (bool, Option<String>) {
        let timeout = Duration::from_secs_f64(self.inner.config.timeouts.pull_seconds);
        let url = format!("{}/api/pull", base_url.trim_end_matches('/'));
        let body = match serde_json::to_vec(&serde_json::json!({ "model": model })) {
            Ok(b) => b,
            Err(_) => return (false, Some("encode pull body".into())),
        };
        let resp = match self
            .inner
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(timeout)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => return (false, Some(truncate(&err.to_string()))),
        };
        if resp.status().as_u16() >= 400 {
            return (false, Some(format!("upstream {}", resp.status().as_u16())));
        }
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        let mut last_status = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(err) => return (false, Some(truncate(&err.to_string()))),
            };
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                        return (false, Some(truncate(err)));
                    }
                    if let Some(status) = value.get("status").and_then(|v| v.as_str()) {
                        last_status = status.to_string();
                    }
                }
            }
        }
        if last_status == "success" {
            (true, None)
        } else if last_status.is_empty() {
            (true, Some("stream ended".into()))
        } else {
            (true, Some(last_status))
        }
    }

    async fn delete_model(&self, base_url: &str, model: &str) -> (bool, Option<String>) {
        let timeout = Duration::from_secs_f64(self.inner.config.timeouts.default_seconds);
        let url = format!("{}/api/delete", base_url.trim_end_matches('/'));
        let body = match serde_json::to_vec(&serde_json::json!({ "model": model })) {
            Ok(b) => b,
            Err(_) => return (false, Some("encode delete body".into())),
        };
        let resp = match self
            .inner
            .client
            .request(reqwest::Method::DELETE, &url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(timeout)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => return (false, Some(truncate(&err.to_string()))),
        };
        match resp.status().as_u16() {
            200 => (true, None),
            404 => (true, Some("absent".into())),
            code => (false, Some(format!("upstream {code}"))),
        }
    }

    async fn set_target(
        &self,
        job_id: &JobId,
        node_id: &str,
        model: &str,
        status: TargetStatus,
        detail: Option<String>,
    ) {
        let snap = {
            let mut state = self.lock();
            let Some(job) = state.jobs.get_mut(job_id) else {
                return;
            };
            let key = Job::target_key(node_id, model);
            if let Some(target) = job.targets.get_mut(&key) {
                target.status = status;
                target.detail = detail;
            }
            job.clone()
        };
        self.persist(&snap).await;
        self.notify(&snap);
    }

    async fn persist(&self, job: &Job) {
        let Some(store) = self.inner.store.clone() else {
            return;
        };
        if let Err(err) = store.save_async(job).await {
            tracing::warn!(job_id = %job.id, error = %err, "job persist failed");
        }
    }

    fn notify(&self, job: &Job) {
        let state = self.lock();
        if let Some(tx) = state.waiters.get(&job.id) {
            let _ = tx.send(job.clone());
        }
    }

    fn observe_terminal(&self, job: &Job) {
        if job.status.is_incomplete() {
            return;
        }
        let observer = self
            .inner
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(observer) = observer {
            observer.job_terminal(job.kind, job.status);
        }
    }

    fn prune_blocking(&self, room_for: usize) {
        let remove = self.prune_ids(room_for);
        self.prune_apply(&remove);
        self.prune_delete_sync(&remove);
    }

    async fn prune(&self, room_for: usize) {
        let remove = self.prune_ids(room_for);
        if remove.is_empty() {
            return;
        }
        self.prune_apply(&remove);
        let Some(store) = self.inner.store.clone() else {
            return;
        };
        match tokio::task::spawn_blocking(move || {
            for id in &remove {
                if let Err(err) = store.delete(id) {
                    tracing::warn!(job_id = %id, error = %err, "job prune delete failed");
                }
            }
        })
        .await
        {
            Ok(()) => {}
            Err(err) => tracing::warn!(error = %err, "job prune delete join failed"),
        }
    }

    fn prune_ids(&self, room_for: usize) -> Vec<JobId> {
        let now = unix_now();
        let ttl = f64::from(self.inner.config.jobs_retention_seconds);
        let max = self.inner.config.jobs_max_retained as usize;
        let mut remove = Vec::new();
        let state = self.lock();
        let done: Vec<&Job> = state
            .jobs
            .values()
            .filter(|j| j.status != JobStatus::Running)
            .collect();
        for job in &done {
            let age_from = job.finished_at.unwrap_or(job.created_at);
            if now - age_from > ttl {
                remove.push(job.id);
            }
        }
        let remaining = state.jobs.len().saturating_sub(remove.len());
        let excess = remaining.saturating_add(room_for).saturating_sub(max);
        if excess > 0 {
            let mut removable: Vec<&Job> = done
                .into_iter()
                .filter(|j| !remove.contains(&j.id))
                .collect();
            removable.sort_by(|a, b| {
                let ta = a.finished_at.unwrap_or(a.created_at);
                let tb = b.finished_at.unwrap_or(b.created_at);
                ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
            });
            for job in removable.into_iter().take(excess) {
                remove.push(job.id);
            }
        }
        remove
    }

    fn prune_apply(&self, remove: &[JobId]) {
        if remove.is_empty() {
            return;
        }
        let mut state = self.lock();
        for id in remove {
            state.jobs.remove(id);
            state.waiters.remove(id);
        }
    }

    fn prune_delete_sync(&self, remove: &[JobId]) {
        let Some(store) = &self.inner.store else {
            return;
        };
        for id in remove {
            if let Err(err) = store.delete(id) {
                tracing::warn!(job_id = %id, error = %err, "job prune delete failed");
            }
        }
    }
}

#[derive(Deserialize)]
struct TagsBody {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    #[serde(default)]
    name: String,
}

fn normalize_models(models: &[String]) -> Result<Vec<String>, OrchestratorError> {
    let out: Vec<String> = models
        .iter()
        .map(|m| normalize_model(m))
        .filter(|m| !m.is_empty())
        .collect();
    if out.is_empty() {
        return Err(OrchestratorError::EmptyModels);
    }
    Ok(out)
}

fn new_job(kind: JobKind, models: Vec<String>, targets: &BTreeMap<String, Vec<NodeId>>) -> Job {
    let pairs = pairs_of(targets);
    let mut nodes: Vec<String> = pairs.iter().map(|(n, _)| n.clone()).collect();
    nodes.sort();
    nodes.dedup();
    let mut map = BTreeMap::new();
    for (node, model) in &pairs {
        map.insert(
            Job::target_key(node, model),
            JobTarget::new(node.clone(), model.clone()),
        );
    }
    Job {
        id: JobId::new(),
        kind,
        status: JobStatus::Running,
        created_at: unix_now(),
        finished_at: None,
        models,
        nodes,
        targets: map,
    }
}

fn pairs_of(targets: &BTreeMap<String, Vec<NodeId>>) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (model, nodes) in targets {
        for node in nodes {
            pairs.push((node.as_str().to_string(), model.clone()));
        }
    }
    pairs
}

fn node_id_targets_of(job: &Job) -> BTreeMap<String, Vec<NodeId>> {
    let mut map: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for t in job.targets.values() {
        if let Ok(id) = NodeId::parse(&t.node) {
            map.entry(t.model.clone()).or_default().push(id);
        }
    }
    map
}

fn dedupe_key(kind: JobKind, models: &[String], pairs: &[(String, String)]) -> DedupeKey {
    let mut models: Vec<String> = models.iter().map(|m| normalize_model(m)).collect();
    models.sort();
    let mut pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(model, node)| (normalize_model(model), node.clone()))
        .collect();
    pairs.sort();
    DedupeKey {
        kind,
        models,
        pairs,
    }
}

fn pairs_from_job(job: &Job) -> Vec<(String, String)> {
    job.targets
        .values()
        .map(|t| (t.model.clone(), t.node.clone()))
        .collect()
}

fn pairs_from_map(targets: &BTreeMap<String, Vec<NodeId>>) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (model, nodes) in targets {
        for node in nodes {
            pairs.push((model.clone(), node.as_str().to_string()));
        }
    }
    pairs
}

fn target_incomplete(job: &Job, node_id: &str, model: &str) -> bool {
    job.targets
        .get(&Job::target_key(node_id, model))
        .is_some_and(|t| t.status.is_incomplete())
}

fn outcome(job: &Job) -> JobOutcome {
    JobOutcome {
        id: job.id,
        status: job.status,
    }
}

fn truncate(s: &str) -> String {
    let mut out: String = s.chars().take(200).collect();
    if s.chars().count() > 200 {
        out.push('…');
    }
    out
}
