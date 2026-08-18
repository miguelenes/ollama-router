//! RunPod interruptible pod fleet manager: create, destroy, reconcile, idle scale-down.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ollama_router_core::cloud::{
    excess_scale_down_order, idle_scale_down_candidates, orphan_reclaim_candidates, CachedOffer,
    CloudProviderHandle, DemandScale, FleetEvents, IdleNodeView, IdlePolicy,
};
use ollama_router_core::config::{Capacity, EnvSource, NodeConfig, OsEnv, RouterConfig};
use ollama_router_core::fleet::{
    CloudInstanceId, FleetState, NodeId, NodeOrigin, Registry, RunpodNodePersist,
};
use ollama_router_core::http_util::rustls_client;
use ollama_router_core::routing::RoutingError;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::client::{RunpodClient, RunpodError};
use crate::selector::{rank_gpu_types, GpuChoice};
use crate::startup::{build_create_request, default_package_urls, BootstrapParams};
use crate::types::Pod;

/// Embedded in pod names so reconcile can recognize owned pods (RunPod has no tags).
pub const MANAGED_BY_MARKER: &str = "or-rp";
const REASON_WAITING_FOR_ENROLL: &str = "waiting_for_enroll";
const REASON_ENROLL_TIMEOUT: &str = "enroll_timeout";
const REASON_TAGS_UNREACHABLE: &str = "tags_unreachable";
const REASON_PUBLIC_URL_BLOCKED: &str = "public_url_blocked";
const REASON_OVER_CAP: &str = "over_cap_cost";

static RUNPOD_UNKNOWN_NODE_ID: LazyLock<NodeId> =
    LazyLock::new(|| NodeId::from_static("runpod-unknown"));

struct EnrollWait {
    ok: bool,
    url: Option<String>,
    detail: String,
}

pub struct RunpodManager {
    inner: Arc<Inner>,
}

#[derive(Clone, Debug)]
struct Recovery {
    id: uuid::Uuid,
    reason: &'static str,
    stage: &'static str,
    status: &'static str,
    detail: Option<String>,
}

struct Inner {
    config: Arc<RouterConfig>,
    client: RunpodClient,
    http: reqwest::Client,
    registry: Arc<Registry>,
    fleet_state: Arc<FleetState>,
    ensure_lock: Mutex<()>,
    demand: std::sync::Mutex<Option<JoinHandle<()>>>,
    events: std::sync::Mutex<Option<Arc<dyn FleetEvents>>>,
    shutdown: CancellationToken,
    recovery: std::sync::Mutex<Option<Recovery>>,
    orphan_first_seen: std::sync::Mutex<HashMap<String, Instant>>,
    cached_offer: std::sync::Mutex<Option<CachedOffer>>,
    cloud_halted: AtomicBool,
}

impl RunpodManager {
    pub fn new(
        config: Arc<RouterConfig>,
        client: RunpodClient,
        registry: Arc<Registry>,
        fleet_state: Arc<FleetState>,
    ) -> Result<Self, RunpodError> {
        Self::with_shutdown(
            config,
            client,
            registry,
            fleet_state,
            CancellationToken::new(),
        )
    }

    pub fn with_shutdown(
        config: Arc<RouterConfig>,
        client: RunpodClient,
        registry: Arc<Registry>,
        fleet_state: Arc<FleetState>,
        shutdown: CancellationToken,
    ) -> Result<Self, RunpodError> {
        let http = rustls_client(Some(Duration::from_secs(5)), Some(Duration::from_secs(5)))
            .map_err(|err| RunpodError::Message(format!("http client: {err}")))?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                client,
                http,
                registry,
                fleet_state,
                ensure_lock: Mutex::new(()),
                demand: std::sync::Mutex::new(None),
                events: std::sync::Mutex::new(None),
                shutdown,
                recovery: std::sync::Mutex::new(None),
                orphan_first_seen: std::sync::Mutex::new(HashMap::new()),
                cached_offer: std::sync::Mutex::new(None),
                cloud_halted: AtomicBool::new(false),
            }),
        })
    }

    pub fn set_halted(&self, halted: bool) {
        self.inner.cloud_halted.store(halted, Ordering::Relaxed);
    }

    pub fn is_halted(&self) -> bool {
        self.inner.cloud_halted.load(Ordering::Relaxed)
    }

    fn creates_blocked(&self) -> bool {
        self.shutdown_cancelled() || self.is_halted()
    }

    fn lock_cached_offer(&self) -> std::sync::MutexGuard<'_, Option<CachedOffer>> {
        self.inner
            .cached_offer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Refresh the best-eligible GPU offer for multi-provider demand ranking.
    /// Catalog failure clears the cache (soft-fail; provider skipped until next tick).
    async fn refresh_cached_offer(&self) {
        let catalog = match self.inner.client.list_catalog_gpus().await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    reason = "catalog_failed",
                    "runpod_cached_offer_soft_fail"
                );
                *self.lock_cached_offer() = None;
                return;
            }
        };
        let ranked = rank_gpu_types(&catalog, &self.inner.config.runpod);
        let offer = ranked.first().map(|choice| CachedOffer {
            hourly_price: choice.on_demand_price,
            vram_gb: Some(choice.vram_gb),
        });
        *self.lock_cached_offer() = offer;
    }

    pub fn set_events(&self, events: Arc<dyn FleetEvents>) {
        *self
            .inner
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(events);
    }

    fn emit(&self, event: &'static str, reason: Option<&str>) {
        let events = self
            .inner
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(events) = events {
            events.cloud_event("runpod", event, reason);
        }
    }

    fn shutdown_cancelled(&self) -> bool {
        self.inner.shutdown.is_cancelled()
    }

    fn lock_demand(&self) -> std::sync::MutexGuard<'_, Option<JoinHandle<()>>> {
        self.inner
            .demand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn abort_demand(&self) {
        let handle = {
            let mut slot = self.lock_demand();
            slot.take()
        };
        let Some(handle) = handle else {
            return;
        };
        handle.abort();
        match handle.await {
            Ok(()) => {}
            Err(err) if err.is_cancelled() => {}
            Err(err) => {
                if err.is_panic() {
                    tracing::error!("runpod_demand_task_panic");
                }
            }
        }
    }

    async fn cancelled_or_sleep(&self, duration: Duration) -> bool {
        tokio::select! {
            biased;
            () = self.inner.shutdown.cancelled() => true,
            () = tokio::time::sleep(duration) => false,
        }
    }

    pub fn node_id_for(pod_id: &str) -> NodeId {
        NodeId::parse(format!("runpod-{pod_id}")).unwrap_or_else(|_| RUNPOD_UNKNOWN_NODE_ID.clone())
    }

    fn router_id(&self) -> String {
        self.inner.config.runpod.router_id(&OsEnv).to_string()
    }

    fn owned_name_prefix(&self) -> String {
        let slug = router_slug(&self.router_id());
        format!("{MANAGED_BY_MARKER}-{slug}-")
    }

    /// Owned when the pod name carries this router's marker (and optionally FleetState).
    fn is_owned(&self, pod: &Pod) -> bool {
        let Some(name) = pod.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return false;
        };
        name.starts_with(&self.owned_name_prefix())
    }

    fn status_is_running(status: &str) -> bool {
        status.eq_ignore_ascii_case("RUNNING")
    }

    fn status_is_active(status: &str) -> bool {
        let s = status.to_ascii_uppercase();
        matches!(
            s.as_str(),
            "RUNNING" | "CREATED" | "STARTING" | "PROVISIONING" | ""
        )
    }

    async fn persist_runpod(
        &self,
        node_id: &NodeId,
        url: &str,
        pod: &Pod,
        gpu_type: &str,
        cost: Option<f64>,
    ) -> Result<(), RunpodError> {
        let pid = pod
            .pod_id()
            .ok_or_else(|| RunpodError::Message("RunPod persist missing pod id".into()))?;
        let pod_id = CloudInstanceId::parse(pid)
            .map_err(|err| RunpodError::Message(format!("runpod persist pod id: {err}")))?;
        let persist = RunpodNodePersist {
            url,
            pod_id: &pod_id,
            gpu_type,
            data_center: pod.data_center(),
            cost_per_hour: cost.or_else(|| pod.cost_per_hour()),
            hostname: pod.name.as_deref(),
        };
        self.inner
            .fleet_state
            .persist_runpod_node_async(node_id.as_str(), persist)
            .await
            .map_err(|err| RunpodError::Message(format!("runpod persist failed: {err}")))
    }

    async fn persist_runpod_log(
        &self,
        node_id: &NodeId,
        url: &str,
        pod: &Pod,
        gpu_type: &str,
        cost: Option<f64>,
    ) {
        if let Err(err) = self.persist_runpod(node_id, url, pod, gpu_type, cost).await {
            tracing::error!(node_id = %node_id, error = %err, "runpod persist failed");
        }
    }

    fn build_node(&self, pod: &Pod, choice: &GpuChoice, url: Option<String>) -> Option<NodeConfig> {
        let pod_id = pod.pod_id()?;
        Some(NodeConfig {
            id: Self::node_id_for(pod_id),
            url,
            capacity_url: None,
            labels: vec!["gpu".into(), "runpod".into(), "spot".into()],
            static_capacity: Capacity {
                vram_gb: Some(choice.vram_gb),
                ram_gb: None,
                gpus: Some(1),
                cpu_cores: None,
            },
            max_inflight: None,
        })
    }

    async fn forget_pod(&self, pod_id: &str) {
        let node_id = Self::node_id_for(pod_id);
        self.inner.registry.remove_runpod(&node_id);
        if let Err(err) = self.inner.fleet_state.remove_async(node_id.as_str()).await {
            tracing::warn!(node_id = %node_id, error = %err, "runpod forget fleet-state remove failed");
        }
    }

    /// Adopt-first ensure: reuse a RUNNING owned pod, else optionally create.
    pub async fn ensure(&self, create: bool) -> Result<Value, RunpodError> {
        if self.creates_blocked() {
            tracing::info!(halted = self.is_halted(), "runpod_ensure_skipped");
            return Ok(json!({"status": "none"}));
        }
        let _lock = self.inner.ensure_lock.lock().await;
        if self.creates_blocked() {
            tracing::info!(halted = self.is_halted(), "runpod_ensure_skipped");
            return Ok(json!({"status": "none"}));
        }
        self.ensure_locked(create)
            .await
            .inspect_err(|_| self.emit("ensure_failed", Some("provision_failed")))
    }

    async fn ensure_locked(&self, create: bool) -> Result<Value, RunpodError> {
        let pods = self.inner.client.list_pods().await?;
        let owned: Vec<_> = pods.into_iter().filter(|p| self.is_owned(p)).collect();
        for pod in &owned {
            if Self::status_is_running(pod.status()) {
                return Ok(self.adopt(pod).await);
            }
        }
        if !create {
            return Ok(json!({"status": "none"}));
        }
        let Some((created, choice)) = self.create_pod().await? else {
            return Ok(json!({"status": "none"}));
        };
        Ok(self.finish_create(&created, &choice).await)
    }

    async fn adopt(&self, pod: &Pod) -> Value {
        let pod_id = pod.pod_id().unwrap_or("unknown");
        let node_id = Self::node_id_for(pod_id);
        let gpu_type = pod.gpu_type().unwrap_or("unknown").to_string();
        tracing::info!(pod_id = %pod_id, gpu_type = %gpu_type, "runpod_adopted");
        let choice = GpuChoice {
            gpu_type_id: gpu_type.clone(),
            vram_gb: 0.0,
            on_demand_price: pod.cost_per_hour().unwrap_or(0.0),
            data_center: pod.data_center().map(str::to_string),
        };
        let Some(node) = self.build_node(pod, &choice, None) else {
            return json!({"status": "none"});
        };
        self.inner.registry.upsert_runpod(node);
        self.persist_runpod_log(&node_id, "", pod, &choice.gpu_type_id, None)
            .await;
        let ready = self.wait_enrolled_and_healthy(&node_id).await;
        if !ready.ok {
            self.terminate_pod_on_enroll_failure(pod_id, &node_id, &ready.detail)
                .await;
        }
        self.persist_runpod_log(
            &node_id,
            ready.url.as_deref().unwrap_or(""),
            pod,
            &choice.gpu_type_id,
            None,
        )
        .await;
        self.emit("adopt", None);
        json!({
            "status": "adopted",
            "node_id": node_id.as_str(),
            "pod_id": pod_id,
            "gpu_type": gpu_type,
            "url": ready.url,
            "enroll": if ready.ok { "ok" } else { "fail" },
            "detail": ready.detail,
        })
    }

    pub async fn create_additional(&self) -> Result<Value, RunpodError> {
        if self.creates_blocked() {
            tracing::info!(halted = self.is_halted(), "runpod_create_skipped");
            return Ok(json!({"status": "none"}));
        }
        let _lock = self.inner.ensure_lock.lock().await;
        if self.creates_blocked() {
            tracing::info!(halted = self.is_halted(), "runpod_create_skipped");
            return Ok(json!({"status": "none"}));
        }
        self.create_additional_locked()
            .await
            .inspect_err(|_| self.emit("ensure_failed", Some("provision_failed")))
    }

    async fn create_additional_locked(&self) -> Result<Value, RunpodError> {
        let max_n = self.inner.config.runpod.auto_scale_max_instances;
        if max_n > 0 {
            let pods = self.inner.client.list_pods().await?;
            let live = pods
                .iter()
                .filter(|p| self.is_owned(p) && Self::status_is_active(p.status()))
                .count() as u32;
            let registered = self
                .inner
                .registry
                .snapshot()
                .iter()
                .filter(|n| n.origin == NodeOrigin::Runpod)
                .count() as u32;
            let count = live.max(registered);
            if count >= max_n {
                tracing::info!(
                    owned_count = count,
                    max_n,
                    "runpod_create_additional_capped"
                );
                return Ok(json!({"status": "none"}));
            }
        }
        let Some((created, choice)) = self.create_pod().await? else {
            return Ok(json!({"status": "none"}));
        };
        Ok(self.finish_create(&created, &choice).await)
    }

    async fn finish_create(&self, pod: &Pod, choice: &GpuChoice) -> Value {
        let pod_id = pod.pod_id().unwrap_or("unknown");
        let node_id = Self::node_id_for(pod_id);
        let Some(node) = self.build_node(pod, choice, None) else {
            return json!({"status": "none"});
        };
        self.inner.registry.upsert_runpod(node);
        self.persist_runpod_log(
            &node_id,
            "",
            pod,
            &choice.gpu_type_id,
            pod.cost_per_hour().or(Some(choice.on_demand_price)),
        )
        .await;
        let ready = self.wait_enrolled_and_healthy(&node_id).await;
        if ready.ok {
            self.persist_runpod_log(
                &node_id,
                ready.url.as_deref().unwrap_or(""),
                pod,
                &choice.gpu_type_id,
                pod.cost_per_hour().or(Some(choice.on_demand_price)),
            )
            .await;
        } else {
            self.terminate_pod_on_enroll_failure(pod_id, &node_id, &ready.detail)
                .await;
        }
        json!({
            "status": "created",
            "node_id": node_id.as_str(),
            "pod_id": pod_id,
            "gpu_type": choice.gpu_type_id,
            "url": ready.url,
            "enroll": if ready.ok { "ok" } else { "fail" },
            "detail": ready.detail,
        })
    }

    async fn wait_enrolled_and_healthy(&self, node_id: &NodeId) -> EnrollWait {
        let timeout =
            Duration::from_secs_f64(self.inner.config.runpod.create_timeout_seconds.max(0.0));
        let poll = Duration::from_secs_f64(self.inner.config.runpod.poll_interval_seconds.max(0.5));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let _detail = match self.try_enroll_ready(node_id).await {
                Ok((url, capacity_url)) => match self.inner.registry.set_node_url(node_id, &url) {
                    Ok(()) => {
                        if let Some(ref cap) = capacity_url {
                            let _ = self.inner.registry.set_capacity_url(node_id, cap);
                        }
                        tracing::info!(node_id = %node_id, "runpod_enrolled");
                        return EnrollWait {
                            ok: true,
                            url: Some(url),
                            detail: "enrolled".into(),
                        };
                    }
                    Err(err) => allowlisted_url_reason(&err),
                },
                Err(detail) => detail,
            };
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(node_id = %node_id, reason = REASON_ENROLL_TIMEOUT, "runpod_enroll_timeout");
                return EnrollWait {
                    ok: false,
                    url: None,
                    detail: REASON_ENROLL_TIMEOUT.into(),
                };
            }
            if self.cancelled_or_sleep(poll).await {
                return EnrollWait {
                    ok: false,
                    url: None,
                    detail: "shutdown".into(),
                };
            }
        }
    }

    async fn terminate_pod_on_enroll_failure(&self, pod_id: &str, node_id: &NodeId, detail: &str) {
        tracing::warn!(
            node_id = %node_id,
            pod_id,
            reason = detail,
            "runpod_enroll_failed_terminate"
        );
        match self.inner.client.delete_pod(pod_id).await {
            Ok(true) => {
                tracing::info!(node_id = %node_id, pod_id, "runpod_enroll_failed_terminated");
            }
            Ok(false) => {
                tracing::warn!(
                    node_id = %node_id,
                    pod_id,
                    reason = "delete_not_found",
                    "runpod_enroll_failed_terminate_miss"
                );
            }
            Err(err) => {
                tracing::warn!(
                    node_id = %node_id,
                    pod_id,
                    error = %err,
                    "runpod_enroll_failed_terminate_error"
                );
            }
        }
    }

    async fn try_enroll_ready(
        &self,
        node_id: &NodeId,
    ) -> Result<(String, Option<String>), &'static str> {
        let url = self
            .inner
            .fleet_state
            .snapshot_hydrate_url(node_id)
            .ok_or(REASON_WAITING_FOR_ENROLL)?;
        let tags_url = format!("{}/api/tags", url.trim_end_matches('/'));
        let resp = self
            .inner
            .http
            .get(&tags_url)
            .send()
            .await
            .map_err(|_| REASON_TAGS_UNREACHABLE)?;
        if !resp.status().is_success() {
            return Err(REASON_TAGS_UNREACHABLE);
        }
        let capacity_url = self
            .inner
            .fleet_state
            .snapshot_hydrate_capacity_url(node_id);
        Ok((url, capacity_url))
    }

    async fn create_pod(&self) -> Result<Option<(Pod, GpuChoice)>, RunpodError> {
        if self.shutdown_cancelled() {
            tracing::info!("runpod_create_skipped_shutdown");
            return Ok(None);
        }
        let catalog = match self.inner.client.list_catalog_gpus().await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(error = %err, reason = "catalog_failed", "runpod_catalog_soft_fail");
                return Ok(None);
            }
        };
        let ranked = rank_gpu_types(&catalog, &self.inner.config.runpod);
        if ranked.is_empty() {
            tracing::warn!(
                min_vram_gb = self.inner.config.runpod.min_vram_gb,
                reason = "no_eligible",
                "runpod_no_eligible_gpu"
            );
            return Ok(None);
        }
        let gpu_type_ids: Vec<String> = ranked.iter().map(|c| c.gpu_type_id.clone()).collect();
        let data_centers: Option<Vec<String>> = {
            let mut dcs: Vec<String> = ranked
                .iter()
                .filter_map(|c| c.data_center.clone())
                .collect();
            dcs.sort();
            dcs.dedup();
            if dcs.is_empty() {
                let allowed = &self.inner.config.runpod.allowed_data_centers;
                if allowed.is_empty() {
                    None
                } else {
                    Some(allowed.clone())
                }
            } else {
                Some(dcs)
            }
        };
        let best = ranked[0].clone();

        // Interruptible first.
        if self.inner.config.runpod.interruptible {
            match self
                .create_one(true, gpu_type_ids.clone(), data_centers.clone(), &best)
                .await
            {
                Ok(pod) => return Ok(Some((pod, best))),
                Err(err) => {
                    if self.shutdown_cancelled() {
                        return Ok(None);
                    }
                    tracing::warn!(error = %err, reason = "interruptible_stockout", "runpod_create_failed");
                    if !self.inner.config.runpod.on_demand_fallback {
                        tracing::info!(reason = "no_on_demand_fallback", "runpod_stockout");
                        return Ok(None);
                    }
                }
            }
        }

        if self.inner.config.runpod.on_demand_fallback || !self.inner.config.runpod.interruptible {
            match self
                .create_one(false, gpu_type_ids, data_centers, &best)
                .await
            {
                Ok(pod) => return Ok(Some((pod, best))),
                Err(err) => {
                    tracing::warn!(error = %err, reason = "on_demand_stockout", "runpod_create_failed");
                    return Err(err);
                }
            }
        }
        Ok(None)
    }

    async fn create_one(
        &self,
        interruptible: bool,
        gpu_type_ids: Vec<String>,
        data_center_ids: Option<Vec<String>>,
        choice: &GpuChoice,
    ) -> Result<Pod, RunpodError> {
        if self.shutdown_cancelled() {
            return Err(RunpodError::Message("shutdown".into()));
        }
        let name = pod_name_for(&self.router_id());
        let cfg = &self.inner.config.runpod;
        let env = OsEnv;
        let (deb_amd64, deb_arm64, tar_amd64, tar_arm64) = default_package_urls(cfg);
        let mut secret_env = BTreeMap::new();
        if let Some(zrok) = env
            .var(&cfg.zrok_enable_token_env)
            .filter(|s| !s.trim().is_empty())
        {
            secret_env.insert(cfg.zrok_enable_token_env.trim().to_string(), zrok);
        }
        if let Some(token) = env
            .var(&cfg.enroll_token_env)
            .filter(|s| !s.trim().is_empty())
        {
            secret_env.insert(cfg.enroll_token_env.trim().to_string(), token);
        }
        let params = BootstrapParams {
            enroll_url: cfg
                .enroll_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            zrok_api_endpoint: self.inner.config.tunnel.api_endpoint(),
            enroll_token_env: cfg.enroll_token_env.trim(),
            package_url: cfg
                .agent_package_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            deb_amd64: &deb_amd64,
            deb_arm64: &deb_arm64,
            tar_amd64: &tar_amd64,
            tar_arm64: &tar_arm64,
            secret_env,
        };
        let request = build_create_request(
            &name,
            interruptible,
            gpu_type_ids,
            data_center_ids,
            cfg,
            &params,
        )
        .map_err(RunpodError::Message)?;
        tracing::info!(
            gpu_type = %choice.gpu_type_id,
            interruptible,
            vram_gb = choice.vram_gb,
            on_demand_price = choice.on_demand_price,
            "runpod_create"
        );
        let created = self.inner.client.create_pod(&request).await?;
        let pod_id = created
            .pod_id()
            .ok_or_else(|| RunpodError::Message("RunPod create returned no pod id".into()))?
            .to_string();
        let mut created = created;
        if created
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            created.name = Some(name.clone());
        }

        // D5: verify actual costPerHr against cap.
        if let Some(cap) = self.inner.config.runpod.max_price_per_hour {
            if let Some(actual) = created.cost_per_hour() {
                if actual > cap {
                    tracing::warn!(
                        pod_id = %pod_id,
                        cost_per_hr = actual,
                        max_price_per_hour = cap,
                        reason = REASON_OVER_CAP,
                        "runpod_over_cap_terminate"
                    );
                    let _ = self.inner.client.delete_pod(&pod_id).await;
                    return Err(RunpodError::Message(REASON_OVER_CAP.into()));
                }
            }
        }

        let node_id = Self::node_id_for(&pod_id);
        if let Err(err) = self
            .persist_runpod(
                &node_id,
                "",
                &created,
                &choice.gpu_type_id,
                created.cost_per_hour().or(Some(choice.on_demand_price)),
            )
            .await
        {
            tracing::error!(node_id = %node_id, error = %err, "runpod persist failed");
            let _ = self.compensate(&pod_id).await;
            return Err(err);
        }
        match self.wait_running(&pod_id).await {
            Ok(pod) => {
                self.emit("create", None);
                Ok(pod)
            }
            Err(err) => {
                if self.compensate(&pod_id).await.is_ok() {
                    self.forget_pod(&pod_id).await;
                }
                Err(err)
            }
        }
    }

    async fn wait_running(&self, pod_id: &str) -> Result<Pod, RunpodError> {
        let timeout = Duration::from_secs_f64(self.inner.config.runpod.create_timeout_seconds);
        let poll = Duration::from_secs_f64(self.inner.config.runpod.poll_interval_seconds.max(0.5));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let pod = self.inner.client.get_pod(pod_id).await?;
            let status = pod.status();
            if status.eq_ignore_ascii_case("EXITED")
                || status.eq_ignore_ascii_case("TERMINATED")
                || status.eq_ignore_ascii_case("FAILED")
                || status.eq_ignore_ascii_case("ERROR")
            {
                return Err(RunpodError::Message(format!(
                    "RunPod pod {pod_id} reached terminal status {status}"
                )));
            }
            if Self::status_is_running(status) {
                tracing::info!(pod_id, "runpod_running");
                return Ok(pod);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RunpodError::Message(format!(
                    "RunPod pod {pod_id} not running within {:.0}s (status={status})",
                    timeout.as_secs_f64()
                )));
            }
            if self.cancelled_or_sleep(poll).await {
                return Err(RunpodError::Message("shutdown".into()));
            }
        }
    }

    async fn compensate(&self, pod_id: &str) -> Result<(), RunpodError> {
        match self.inner.client.delete_pod(pod_id).await {
            Ok(_) => Ok(()),
            Err(err) => {
                tracing::error!(pod_id, error = %err, "runpod_create_compensation_failed");
                Err(err)
            }
        }
    }

    pub async fn destroy_all_owned(&self) -> Value {
        let _lock = self.inner.ensure_lock.lock().await;
        let timeout = Duration::from_secs_f64(self.inner.config.runpod.destroy_timeout_seconds);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut ids: BTreeMap<String, ()> = BTreeMap::new();
        let nodes = match self.inner.fleet_state.load_async().await {
            Ok(all) => all
                .into_iter()
                .filter(|(_, e)| e.managed_by.as_deref() == Some("runpod"))
                .collect(),
            Err(_) => self.inner.fleet_state.snapshot_runpod_nodes(),
        };
        for entry in nodes.values() {
            if let Some(id) = entry.runpod_pod_id.clone() {
                ids.insert(id, ());
            }
        }
        if let Ok(live) = self.inner.client.list_pods().await {
            for pod in live.into_iter().filter(|p| self.is_owned(p)) {
                if let Some(id) = pod.pod_id() {
                    ids.insert(id.to_string(), ());
                }
            }
        }
        let mut deleted = Vec::new();
        let mut failed = Vec::new();
        for pod_id in ids.into_keys() {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(pod_id = %pod_id, "runpod_destroy_timeout");
                failed.push(pod_id);
                continue;
            }
            match self.inner.client.delete_pod(&pod_id).await {
                Ok(_) => {
                    tracing::info!(pod_id = %pod_id, "runpod_destroyed");
                    deleted.push(pod_id.clone());
                    self.forget_pod(&pod_id).await;
                    self.emit("destroy", None);
                }
                Err(err) => {
                    tracing::error!(pod_id = %pod_id, error = %err, "runpod_destroy_failed");
                    self.emit("destroy", Some("destroy_failed"));
                    failed.push(pod_id);
                }
            }
        }
        json!({"deleted": deleted, "failed": failed})
    }

    pub async fn status(&self) -> Value {
        let pods = match self.inner.client.list_pods().await {
            Ok(list) => list
                .into_iter()
                .filter(|p| self.is_owned(p))
                .collect::<Vec<_>>(),
            Err(err) => {
                return json!({"error": err.to_string().chars().take(200).collect::<String>(), "pods": []});
            }
        };
        let fleet = self.inner.fleet_state.snapshot_runpod_nodes();
        let out: Vec<Value> = pods
            .iter()
            .map(|pod| {
                let pod_id = pod.pod_id().unwrap_or("unknown");
                let node_id = Self::node_id_for(pod_id);
                let entry = fleet.get(node_id.as_str());
                json!({
                    "node_id": node_id.as_str(),
                    "pod_id": pod_id,
                    "gpu_type": pod.gpu_type(),
                    "data_center": pod.data_center(),
                    "cost_per_hr": pod.cost_per_hour(),
                    "status": pod.status(),
                    "url": entry.and_then(|e| e.url.clone()),
                    "local_access_url": entry.and_then(|e| e.local_access_url.clone()),
                    "mode": "enroll",
                    "in_registry": self.inner.registry.get(&node_id).is_some(),
                })
            })
            .collect();
        json!({"enabled": self.inner.config.runpod.enabled, "pods": out})
    }

    pub fn recovery(&self) -> Value {
        let recovery = self
            .inner
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match recovery.as_ref() {
            Some(item) => json!({
                "id": item.id,
                "reason": item.reason,
                "stage": item.stage,
                "status": item.status,
                "detail": item.detail,
            }),
            None => Value::Null,
        }
    }

    fn set_recovery(&self, recovery: Recovery) {
        *self
            .inner
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(recovery);
    }

    fn lock_orphan_first_seen(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.inner
            .orphan_first_seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn registry_runpod_pod_ids(&self) -> HashSet<String> {
        self.inner
            .registry
            .snapshot()
            .into_iter()
            .filter(|node| node.origin == NodeOrigin::Runpod)
            .filter_map(|node| node.id.as_str().strip_prefix("runpod-").map(str::to_string))
            .collect()
    }

    fn idle_views(&self, pods: &[Pod], now: Instant) -> Vec<IdleNodeView> {
        pods.iter()
            .filter_map(|pod| {
                let pid = pod.pod_id()?;
                let node_id = Self::node_id_for(pid);
                let snap = self.inner.registry.get(&node_id)?;
                let instance_id = CloudInstanceId::parse(pid).ok()?;
                let registered_at = self.inner.registry.registered_at(&node_id).unwrap_or(now);
                let last_client_request_at = self.inner.registry.last_client_request_at(&node_id);
                Some(IdleNodeView {
                    node_id,
                    instance_id,
                    origin: snap.origin,
                    inflight: snap.inflight,
                    registered_at,
                    last_client_request_at,
                })
            })
            .collect()
    }

    fn idle_policy(&self, min_n: u32) -> IdlePolicy {
        IdlePolicy {
            idle_timeout: Duration::from_secs_f64(
                self.inner.config.runpod.idle_timeout_seconds.max(0.0),
            ),
            grace_after_create: Duration::from_secs_f64(
                self.inner
                    .config
                    .runpod
                    .idle_grace_after_create_seconds
                    .max(0.0),
            ),
            min_instances: min_n,
            min_lifetime: Duration::from_secs_f64(
                self.inner.config.runpod.min_lifetime_seconds.max(0.0),
            ),
        }
    }

    async fn drain_and_destroy(
        &self,
        pod: &Pod,
        node_id: &NodeId,
        pod_id: &str,
        require_idle: Option<(Instant, IdlePolicy)>,
    ) -> bool {
        if !self.inner.registry.set_draining(node_id, true) {
            return false;
        }
        if self.inner.registry.inflight(node_id) > 0 {
            self.inner.registry.set_draining(node_id, false);
            return false;
        }
        if let Some((now, policy)) = require_idle {
            let still_idle = self
                .idle_views(std::slice::from_ref(pod), now)
                .first()
                .is_some_and(|view| {
                    !idle_scale_down_candidates(
                        std::slice::from_ref(view),
                        now,
                        IdlePolicy {
                            min_instances: 0,
                            ..policy
                        },
                        NodeOrigin::Runpod,
                    )
                    .is_empty()
                });
            if !still_idle {
                self.inner.registry.set_draining(node_id, false);
                return false;
            }
        }
        match self.inner.client.delete_pod(pod_id).await {
            Ok(_) => {
                self.forget_pod(pod_id).await;
                true
            }
            Err(err) => {
                tracing::error!(
                    node_id = %node_id,
                    pod_id,
                    error = %err,
                    "runpod_scale_down_destroy_failed"
                );
                false
            }
        }
    }

    async fn reclaim_orphans(&self, owned: &[Pod]) {
        if !self.inner.config.runpod.orphan_reclaim_enabled {
            return;
        }
        let fleet_ids: HashSet<String> = self
            .inner
            .fleet_state
            .snapshot_runpod_nodes()
            .values()
            .filter_map(|entry| entry.runpod_pod_id.clone())
            .collect();
        let registry_ids = self.registry_runpod_pod_ids();
        let owned_ids: Vec<CloudInstanceId> = owned
            .iter()
            .filter_map(|pod| CloudInstanceId::parse(pod.pod_id()?).ok())
            .collect();
        let owned_keys: HashSet<String> =
            owned_ids.iter().map(|id| id.as_str().to_string()).collect();
        let now = Instant::now();
        let first_seen = {
            let mut map = self.lock_orphan_first_seen();
            for id in &owned_ids {
                let key = id.as_str();
                if !fleet_ids.contains(key) && !registry_ids.contains(key) {
                    map.entry(key.to_string()).or_insert(now);
                }
            }
            map.retain(|key, _| {
                owned_keys.contains(key) && !fleet_ids.contains(key) && !registry_ids.contains(key)
            });
            map.clone()
        };
        let grace = Duration::from_secs_f64(
            self.inner
                .config
                .runpod
                .orphan_reclaim_grace_seconds
                .max(0.0),
        );
        let candidates = orphan_reclaim_candidates(
            &owned_ids,
            &fleet_ids,
            &registry_ids,
            &first_seen,
            now,
            grace,
        );
        let Some(victim) = candidates.first() else {
            return;
        };
        match self.inner.client.delete_pod(victim.as_str()).await {
            Ok(_) => {
                tracing::info!(pod_id = %victim, "runpod_orphan_reclaimed");
                self.forget_pod(victim.as_str()).await;
                self.lock_orphan_first_seen().remove(victim.as_str());
                self.emit("destroy", None);
            }
            Err(err) => {
                tracing::error!(pod_id = %victim, error = %err, "runpod_orphan_reclaim_failed");
            }
        }
    }

    /// Terminate interrupted/exited managed pods; replace only when below floor.
    async fn cleanup_interruptions(&self, owned: &[Pod]) -> u32 {
        let mut terminated = 0u32;
        for pod in owned {
            if self.shutdown_cancelled() {
                break;
            }
            let status = pod.status();
            if Self::status_is_running(status) || status.eq_ignore_ascii_case("CREATED") {
                continue;
            }
            // Non-RUNNING managed → terminate permanently.
            let Some(pid) = pod.pod_id() else {
                continue;
            };
            tracing::info!(pod_id = pid, status, "runpod_interrupt_cleanup");
            match self.inner.client.delete_pod(pid).await {
                Ok(_) => {
                    self.forget_pod(pid).await;
                    terminated += 1;
                    self.emit("destroy", None);
                }
                Err(err) => {
                    tracing::error!(pod_id = pid, error = %err, "runpod_interrupt_cleanup_failed");
                }
            }
        }
        terminated
    }

    pub async fn reconcile(&self) {
        if self.shutdown_cancelled() {
            return;
        }
        let live = match self.inner.client.list_pods().await {
            Ok(v) => v,
            Err(err) => {
                tracing::error!(error = %err, "runpod_reconcile_list_failed");
                return;
            }
        };
        let owned: Vec<_> = live.into_iter().filter(|p| self.is_owned(p)).collect();
        let live_ids: HashSet<String> = owned
            .iter()
            .filter_map(|p| p.pod_id().map(str::to_string))
            .collect();
        let fleet = self.inner.fleet_state.snapshot_runpod_nodes();
        for (node_id, entry) in fleet {
            let Some(pid) = entry.runpod_pod_id.clone() else {
                continue;
            };
            if !live_ids.contains(&pid) {
                tracing::info!(node_id = %node_id, pod_id = %pid, "runpod_reconcile_gone");
                if let Ok(nid) = NodeId::parse(&node_id) {
                    self.inner.registry.remove_runpod(&nid);
                }
                if let Err(err) = self.inner.fleet_state.remove_async(&node_id).await {
                    tracing::warn!(node_id = %node_id, error = %err, "runpod reconcile remove failed");
                }
            }
        }

        let _ = self.cleanup_interruptions(&owned).await;
        // Refresh owned after interruptions.
        let live = match self.inner.client.list_pods().await {
            Ok(v) => v,
            Err(_) => return,
        };
        let owned: Vec<_> = live.into_iter().filter(|p| self.is_owned(p)).collect();

        for pod in &owned {
            if self.shutdown_cancelled() {
                return;
            }
            if !Self::status_is_running(pod.status()) {
                continue;
            }
            let Some(pid) = pod.pod_id() else {
                continue;
            };
            let node_id = Self::node_id_for(pid);
            if self.inner.registry.get(&node_id).is_none() {
                let choice = GpuChoice {
                    gpu_type_id: pod.gpu_type().unwrap_or("unknown").to_string(),
                    vram_gb: 0.0,
                    on_demand_price: pod.cost_per_hour().unwrap_or(0.0),
                    data_center: pod.data_center().map(str::to_string),
                };
                if let Some(node) = self.build_node(pod, &choice, None) {
                    self.inner.registry.upsert_runpod(node);
                    self.persist_runpod_log(&node_id, "", pod, &choice.gpu_type_id, None)
                        .await;
                    self.emit("adopt", None);
                }
            }
        }
        if self.shutdown_cancelled() {
            return;
        }
        self.reclaim_orphans(&owned).await;
        if self.shutdown_cancelled() || !self.inner.config.runpod.auto_scale {
            return;
        }
        let min_n = self.inner.config.runpod.auto_scale_min_instances;
        let max_n = self.inner.config.runpod.auto_scale_max_instances;
        let in_flight: Vec<_> = owned
            .iter()
            .filter(|p| Self::status_is_active(p.status()))
            .cloned()
            .collect();
        let mut count = in_flight.len() as u32;
        while min_n > 0 && count < min_n {
            if self.shutdown_cancelled() {
                return;
            }
            match self.create_additional().await {
                Ok(_) => count += 1,
                Err(err) => {
                    tracing::error!(error = %err, "runpod_reconcile_scale_up_failed");
                    break;
                }
            }
        }
        if self.shutdown_cancelled() {
            return;
        }
        if self.inner.config.runpod.idle_scale_down_enabled {
            self.idle_scale_down(&in_flight, min_n).await;
        }
        if self.shutdown_cancelled() {
            return;
        }
        self.trim_excess(&in_flight, min_n, max_n).await;
    }

    async fn idle_scale_down(&self, in_flight: &[Pod], min_n: u32) {
        let now = Instant::now();
        let views = self.idle_views(in_flight, now);
        let policy = self.idle_policy(min_n);
        for cand in idle_scale_down_candidates(&views, now, policy, NodeOrigin::Runpod) {
            let Some(pod) = in_flight
                .iter()
                .find(|p| p.pod_id() == Some(cand.instance_id.as_str()))
            else {
                continue;
            };
            if self
                .drain_and_destroy(
                    pod,
                    &cand.node_id,
                    cand.instance_id.as_str(),
                    Some((now, policy)),
                )
                .await
            {
                tracing::info!(
                    node_id = %cand.node_id,
                    pod_id = %cand.instance_id,
                    "runpod_idle_scale_down"
                );
                self.emit("idle", None);
            }
        }
    }

    async fn trim_excess(&self, in_flight: &[Pod], min_n: u32, max_n: u32) {
        if max_n == 0 {
            return;
        }
        let remaining: Vec<_> = in_flight
            .iter()
            .filter(|p| {
                p.pod_id()
                    .is_some_and(|id| self.inner.registry.get(&Self::node_id_for(id)).is_some())
            })
            .cloned()
            .collect();
        let mut count = remaining.len() as u32;
        if count <= max_n {
            return;
        }
        let now = Instant::now();
        let views = self.idle_views(&remaining, now);
        for cand in excess_scale_down_order(&views, NodeOrigin::Runpod) {
            if count <= max_n || count <= min_n {
                break;
            }
            let Some(pod) = remaining
                .iter()
                .find(|p| p.pod_id() == Some(cand.instance_id.as_str()))
            else {
                continue;
            };
            if self
                .drain_and_destroy(pod, &cand.node_id, cand.instance_id.as_str(), None)
                .await
            {
                tracing::info!(
                    node_id = %cand.node_id,
                    pod_id = %cand.instance_id,
                    "runpod_excess_scale_down"
                );
                self.emit("destroy", None);
                count = count.saturating_sub(1);
            }
        }
    }

    pub async fn run_reconcile_loop(self) {
        let interval =
            Duration::from_secs_f64(self.inner.config.runpod.poll_interval_seconds.max(1.0));
        loop {
            if self.shutdown_cancelled() {
                tracing::info!("runpod_reconcile_loop_stopped");
                return;
            }
            self.refresh_cached_offer().await;
            self.reconcile().await;
            if self.cancelled_or_sleep(interval).await {
                tracing::info!("runpod_reconcile_loop_stopped");
                return;
            }
        }
    }
}

impl CloudProviderHandle for RunpodManager {
    fn provider(&self) -> &'static str {
        "runpod"
    }

    fn request_scale_up(&self, reason: RoutingError) {
        DemandScale::request_scale_up(self, reason);
    }

    fn cached_best_offer(&self) -> Option<CachedOffer> {
        *self.lock_cached_offer()
    }

    fn below_ceiling(&self) -> bool {
        if self.is_halted() {
            return false;
        }
        let max_n = self.inner.config.runpod.auto_scale_max_instances;
        if max_n == 0 {
            return true;
        }
        let owned = self
            .inner
            .registry
            .snapshot()
            .iter()
            .filter(|n| n.origin == NodeOrigin::Runpod)
            .count() as u32;
        owned < max_n
    }
}

impl DemandScale for RunpodManager {
    fn request_scale_up(&self, reason: RoutingError) {
        let reason_code = reason.as_reason_code();
        if self.creates_blocked() {
            tracing::info!(
                reason = reason_code,
                halted = self.is_halted(),
                "runpod_demand_scale_up_skipped"
            );
            return;
        }
        if !self.inner.config.runpod.auto_scale {
            tracing::info!(reason = reason_code, "runpod_demand_scale_up_skipped");
            return;
        }
        let max_n = self.inner.config.runpod.auto_scale_max_instances;
        if max_n > 0 {
            let owned = self
                .inner
                .registry
                .snapshot()
                .iter()
                .filter(|n| n.origin == NodeOrigin::Runpod)
                .count() as u32;
            if owned >= max_n {
                tracing::info!(
                    reason = reason_code,
                    owned_count = owned,
                    max_n,
                    "runpod_demand_scale_up_capped"
                );
                return;
            }
        }
        let mut slot = self.lock_demand();
        if self.shutdown_cancelled() {
            return;
        }
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            tracing::info!(reason = reason_code, "runpod_demand_scale_up_coalesced");
            return;
        }
        let recovery_id = uuid::Uuid::new_v4();
        self.set_recovery(Recovery {
            id: recovery_id,
            reason: reason_code,
            stage: "provisioning",
            status: "pending",
            detail: None,
        });
        let inner = self.inner.clone();
        let shutdown = self.inner.shutdown.clone();
        self.emit("demand", Some(reason_code));
        *slot = Some(tokio::spawn(async move {
            let mgr = RunpodManager { inner };
            if shutdown.is_cancelled() {
                return;
            }
            mgr.set_recovery(Recovery {
                id: recovery_id,
                reason: reason_code,
                stage: "provisioning",
                status: "running",
                detail: None,
            });
            tracing::info!(reason = reason_code, "runpod_demand_scale_up");
            match mgr.create_additional().await {
                Ok(result) => {
                    let status = result
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("none");
                    mgr.set_recovery(Recovery {
                        id: recovery_id,
                        reason: reason_code,
                        stage: if status == "created" {
                            "enrollment"
                        } else {
                            "provisioning"
                        },
                        status: if status == "created" {
                            "ready"
                        } else {
                            "failed"
                        },
                        detail: result
                            .get("detail")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
                Err(err) if !shutdown.is_cancelled() => {
                    mgr.set_recovery(Recovery {
                        id: recovery_id,
                        reason: reason_code,
                        stage: "provisioning",
                        status: "failed",
                        detail: Some(err.to_string()),
                    });
                    tracing::error!(reason = reason_code, error = %err, "runpod_demand_ensure_failed");
                }
                Err(_) => {}
            }
        }));
    }
}

impl Clone for RunpodManager {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

fn router_slug(router_id: &str) -> String {
    let slug: String = router_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    slug.chars().take(12).collect()
}

fn pod_name_for(router_id: &str) -> String {
    let slug = router_slug(router_id);
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(seq);
    format!("{MANAGED_BY_MARKER}-{slug}-{nanos:x}{seq:x}")
}

fn allowlisted_url_reason(err: &str) -> &'static str {
    if err.contains(REASON_PUBLIC_URL_BLOCKED) {
        REASON_PUBLIC_URL_BLOCKED
    } else {
        REASON_TAGS_UNREACHABLE
    }
}
