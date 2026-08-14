//! Verda spot fleet manager: create, adopt, destroy, reconcile, idle scale-down.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ollama_router_core::cloud::{
    idle_scale_down_candidates, DemandScale, FleetEvents, IdleNodeView, IdlePolicy,
};
use ollama_router_core::config::{Capacity, EnvSource, NodeConfig, OsEnv, RouterConfig};
use ollama_router_core::fleet::{
    FleetState, NodeId, NodeOrigin, Registry, VerdaInstanceId, VerdaNodePersist,
};
use ollama_router_core::routing::RoutingError;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::client::{VerdaClient, VerdaError};
use crate::images::pick_ubuntu24_nvidia_docker_image;
use crate::keys::{companion_private_key, ensure_ssh_key_id};
use crate::selector::{rank_candidates, SpotChoice};
use crate::startup::{agent_init_script, default_package_urls, StartupScriptParams};
use crate::types::Instance;

pub const MANAGED_BY: &str = "ollama-router";
const DESCRIPTION: &str = "ollama-router managed spot";
const TERMINAL: &[&str] = &["failed", "error", "deleted", "terminated"];
const EVICTED: &[&str] = &["discontinued", "evicted"];
const REASON_WAITING_FOR_ENROLL: &str = "waiting_for_enroll";
const REASON_ENROLL_TIMEOUT: &str = "enroll_timeout";
const REASON_TAGS_UNREACHABLE: &str = "tags_unreachable";
const REASON_PUBLIC_URL_BLOCKED: &str = "public_url_blocked";

static VERDA_UNKNOWN_NODE_ID: LazyLock<NodeId> =
    LazyLock::new(|| NodeId::from_static("verda-unknown"));

struct EnrollWait {
    ok: bool,
    url: Option<String>,
    detail: String,
}

pub struct VerdaManager {
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
    client: VerdaClient,
    http: reqwest::Client,
    registry: Arc<Registry>,
    fleet_state: Arc<FleetState>,
    ensure_lock: Mutex<()>,
    startup_script_id: Mutex<Option<String>>,
    demand: std::sync::Mutex<Option<JoinHandle<()>>>,
    events: std::sync::Mutex<Option<Arc<dyn FleetEvents>>>,
    shutdown: CancellationToken,
    recovery: std::sync::Mutex<Option<Recovery>>,
}

impl VerdaManager {
    pub fn new(
        config: Arc<RouterConfig>,
        client: VerdaClient,
        registry: Arc<Registry>,
        fleet_state: Arc<FleetState>,
    ) -> Self {
        Self::with_shutdown(
            config,
            client,
            registry,
            fleet_state,
            CancellationToken::new(),
        )
    }

    /// Same as [`Self::new`], sharing a process-wide shutdown token.
    pub fn with_shutdown(
        config: Arc<RouterConfig>,
        client: VerdaClient,
        registry: Arc<Registry>,
        fleet_state: Arc<FleetState>,
        shutdown: CancellationToken,
    ) -> Self {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            inner: Arc::new(Inner {
                config,
                client,
                http,
                registry,
                fleet_state,
                ensure_lock: Mutex::new(()),
                startup_script_id: Mutex::new(None),
                demand: std::sync::Mutex::new(None),
                events: std::sync::Mutex::new(None),
                shutdown,
                recovery: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Attach fleet-event metrics (binary). Replaces any previous hook.
    pub fn set_events(&self, events: Arc<dyn FleetEvents>) {
        *self
            .inner
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(events);
    }

    fn emit(&self, event: &'static str) {
        let events = self
            .inner
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(events) = events {
            events.verda_event(event);
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

    /// Abort a coalesced `create_additional` task and wait for it to finish.
    ///
    /// Drops the demand mutex before awaiting. Call after the process token is
    /// cancelled and before [`Self::destroy_all_owned`].
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
                    tracing::error!("verda_demand_task_panic");
                }
            }
        }
    }

    /// Sleep `duration`, or return immediately when the process is shutting down.
    ///
    /// Returns `true` if shutdown won.
    async fn cancelled_or_sleep(&self, duration: Duration) -> bool {
        tokio::select! {
            biased;
            () = self.inner.shutdown.cancelled() => true,
            () = tokio::time::sleep(duration) => false,
        }
    }

    pub fn node_id_for(instance_id: &str) -> NodeId {
        NodeId::parse(format!("verda-{instance_id}"))
            .unwrap_or_else(|_| VERDA_UNKNOWN_NODE_ID.clone())
    }

    fn router_id(&self) -> String {
        self.inner.config.verda.router_id(&OsEnv).to_string()
    }

    fn managed_tags(&self) -> Value {
        json!([
            {"key": "managed_by", "value": MANAGED_BY},
            {"key": "router_id", "value": self.router_id()},
        ])
    }

    fn is_owned(&self, instance: &Instance) -> bool {
        let tags = instance.tag_map();
        let Some(managed) = tags.get("managed_by").copied() else {
            return false;
        };
        if managed.starts_with("illumination-") || managed != MANAGED_BY {
            return false;
        }
        let rid = self.router_id();
        match tags.get("router_id") {
            None => true,
            Some(v) => *v == rid,
        }
    }

    fn private_key_file(&self) -> Option<String> {
        if let Some(p) = self.inner.config.verda.ssh_private_key_file.as_deref() {
            return Some(p.to_string());
        }
        self.inner
            .config
            .verda
            .ssh_public_key_file
            .as_deref()
            .map(|p| {
                companion_private_key(std::path::Path::new(p))
                    .to_string_lossy()
                    .into()
            })
    }

    async fn persist_verda(
        &self,
        node_id: &NodeId,
        url: &str,
        instance: &Instance,
        location: &str,
        instance_type: &str,
        spot_price: Option<f64>,
    ) {
        let Some(iid) = instance.instance_id_value() else {
            return;
        };
        let Ok(instance_id) = VerdaInstanceId::parse(iid) else {
            return;
        };
        let persist = VerdaNodePersist {
            url,
            instance_id: &instance_id,
            location,
            instance_type,
            os_volume_id: instance.os_volume_id.as_deref(),
            spot_price_per_hour: spot_price,
            hostname: instance.hostname.as_deref(),
        };
        if let Err(err) = self
            .inner
            .fleet_state
            .persist_verda_node_async(node_id.as_str(), persist)
            .await
        {
            tracing::error!(node_id = %node_id, error = %err, "verda persist failed");
        }
    }

    fn build_node(
        &self,
        instance: &Instance,
        choice: &SpotChoice,
        url: Option<String>,
    ) -> Option<NodeConfig> {
        let instance_id = instance.instance_id_value()?;
        Some(NodeConfig {
            id: Self::node_id_for(instance_id),
            url,
            capacity_url: None,
            labels: vec!["gpu".into(), "verda".into(), "spot".into()],
            static_capacity: Capacity {
                vram_gb: choice.gpu_memory_gb,
                ram_gb: None,
                gpus: Some(choice.gpus),
                cpu_cores: None,
            },
            max_inflight: None,
        })
    }

    async fn forget_instance(&self, instance_id: &str) {
        let node_id = Self::node_id_for(instance_id);
        self.inner.registry.remove_verda(&node_id);
        if let Err(err) = self.inner.fleet_state.remove_async(node_id.as_str()).await {
            tracing::warn!(node_id = %node_id, error = %err, "verda forget fleet-state remove failed");
        }
    }

    pub async fn ensure(&self, create: bool) -> Result<Value, VerdaError> {
        if self.shutdown_cancelled() {
            tracing::info!("verda_ensure_skipped_shutdown");
            return Ok(json!({"status": "none"}));
        }
        let _lock = self.inner.ensure_lock.lock().await;
        if self.shutdown_cancelled() {
            tracing::info!("verda_ensure_skipped_shutdown");
            return Ok(json!({"status": "none"}));
        }
        self.ensure_locked(create)
            .await
            .inspect_err(|_| self.emit("ensure_failed"))
    }

    async fn ensure_locked(&self, create: bool) -> Result<Value, VerdaError> {
        let instances = self.inner.client.list_instances().await?;
        let owned: Vec<_> = instances.into_iter().filter(|i| self.is_owned(i)).collect();
        for instance in &owned {
            let status = instance
                .status
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            if TERMINAL.contains(&status.as_str()) || EVICTED.contains(&status.as_str()) {
                if let Some(id) = instance.instance_id_value() {
                    self.forget_instance(id).await;
                }
                continue;
            }
            if status == "running" {
                return Ok(self.adopt(instance).await);
            }
        }
        if !create {
            return Ok(json!({"status": "none"}));
        }
        let key = self.private_key_file();
        let Some((created, choice)) = self.create_instance(key.as_deref()).await? else {
            return Ok(json!({"status": "none"}));
        };
        Ok(self.finish_create(&created, &choice).await)
    }

    pub async fn create_additional(&self) -> Result<Value, VerdaError> {
        if self.shutdown_cancelled() {
            tracing::info!("verda_create_skipped_shutdown");
            return Ok(json!({"status": "none"}));
        }
        let _lock = self.inner.ensure_lock.lock().await;
        if self.shutdown_cancelled() {
            tracing::info!("verda_create_skipped_shutdown");
            return Ok(json!({"status": "none"}));
        }
        self.create_additional_locked()
            .await
            .inspect_err(|_| self.emit("ensure_failed"))
    }

    async fn create_additional_locked(&self) -> Result<Value, VerdaError> {
        let key = self.private_key_file();
        let Some((created, choice)) = self.create_instance(key.as_deref()).await? else {
            return Ok(json!({"status": "none"}));
        };
        let result = self.finish_create(&created, &choice).await;
        Ok(result)
    }

    async fn adopt(&self, instance: &Instance) -> Value {
        let instance_id = instance.instance_id_value().unwrap_or("unknown");
        let instance_type = instance.instance_type.clone().unwrap_or_default();
        let location = instance.location_value().unwrap_or("").to_string();
        let node_id = Self::node_id_for(instance_id);
        tracing::info!(
            instance_type = %instance_type,
            location_code = %location,
            "verda_adopted"
        );
        let choice = SpotChoice {
            instance_type: instance_type.clone(),
            location_code: location.clone(),
            spot_price: 0.0,
            gpu_memory_gb: None,
            gpus: 1,
            currency: None,
        };
        let Some(node) = self.build_node(instance, &choice, None) else {
            return json!({"status": "none"});
        };
        self.inner.registry.upsert_verda(node);
        self.persist_verda(&node_id, "", instance, &location, &instance_type, None)
            .await;
        let ready = self.wait_enrolled_and_healthy(&node_id).await;
        self.persist_verda(
            &node_id,
            ready.url.as_deref().unwrap_or(""),
            instance,
            &location,
            &instance_type,
            None,
        )
        .await;
        self.emit("adopt");
        json!({
            "status": "adopted",
            "node_id": node_id.as_str(),
            "instance_id": instance_id,
            "instance_type": instance_type,
            "location_code": location,
            "url": ready.url,
            "enroll": if ready.ok { "ok" } else { "fail" },
            "detail": ready.detail,
        })
    }

    async fn finish_create(&self, instance: &Instance, choice: &SpotChoice) -> Value {
        let instance_id = instance.instance_id_value().unwrap_or("unknown");
        let node_id = Self::node_id_for(instance_id);
        let Some(node) = self.build_node(instance, choice, None) else {
            return json!({"status": "none"});
        };
        self.inner.registry.upsert_verda(node);
        self.persist_verda(
            &node_id,
            "",
            instance,
            &choice.location_code,
            &choice.instance_type,
            Some(choice.spot_price),
        )
        .await;
        let ready = self.wait_enrolled_and_healthy(&node_id).await;
        if ready.ok {
            self.persist_verda(
                &node_id,
                ready.url.as_deref().unwrap_or(""),
                instance,
                &choice.location_code,
                &choice.instance_type,
                Some(choice.spot_price),
            )
            .await;
        }
        json!({
            "status": "created",
            "node_id": node_id.as_str(),
            "instance_id": instance_id,
            "instance_type": choice.instance_type,
            "location_code": choice.location_code,
            "url": ready.url,
            "enroll": if ready.ok { "ok" } else { "fail" },
            "detail": ready.detail,
        })
    }

    async fn wait_enrolled_and_healthy(&self, node_id: &NodeId) -> EnrollWait {
        let timeout =
            Duration::from_secs_f64(self.inner.config.verda.create_timeout_seconds.max(0.0));
        let poll = Duration::from_secs_f64(self.inner.config.verda.poll_interval_seconds.max(0.5));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let _detail = match self.try_enroll_ready(node_id).await {
                Ok((url, capacity_url)) => match self.inner.registry.set_node_url(node_id, &url) {
                    Ok(()) => {
                        if let Some(ref cap) = capacity_url {
                            let _ = self.inner.registry.set_capacity_url(node_id, cap);
                        }
                        tracing::info!(node_id = %node_id, "verda_enrolled");
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
                tracing::warn!(node_id = %node_id, reason = REASON_ENROLL_TIMEOUT, "verda_enroll_timeout");
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

    async fn create_instance(
        &self,
        private_key_file: Option<&str>,
    ) -> Result<Option<(Instance, SpotChoice)>, VerdaError> {
        if self.shutdown_cancelled() {
            tracing::info!("verda_create_skipped_shutdown");
            return Ok(None);
        }
        let availability = self.inner.client.get_instance_availability().await?;
        let instance_types = self.inner.client.get_instance_types().await?;
        let images = self.inner.client.get_images().await?;
        let ranked = rank_candidates(&availability, &instance_types, &self.inner.config.verda);
        if ranked.is_empty() {
            tracing::warn!(
                min_vram_gb = self.inner.config.verda.min_vram_gb,
                "verda_no_eligible_spot"
            );
            return Ok(None);
        }
        let attempts = self.inner.config.verda.create_retries + 1;
        let mut last_err = VerdaError::Message("no candidates".into());
        for attempt in 0..attempts as usize {
            let Some(choice) = ranked.get(attempt).cloned() else {
                break;
            };
            tracing::info!(
                attempt = attempt + 1,
                instance_type = %choice.instance_type,
                location_code = %choice.location_code,
                spot_price = choice.spot_price,
                gpu_memory_gb = choice.gpu_memory_gb,
                "verda_pick"
            );
            match self
                .create_one(&choice, &instance_types, &images, private_key_file)
                .await
            {
                Ok(instance) => return Ok(Some((instance, choice))),
                Err(err) => {
                    if self.shutdown_cancelled() {
                        tracing::info!(
                            instance_type = %choice.instance_type,
                            "verda_create_aborted_shutdown"
                        );
                        return Ok(None);
                    }
                    tracing::warn!(
                        instance_type = %choice.instance_type,
                        attempt = attempt + 1,
                        error = %err,
                        "verda_create_failed"
                    );
                    last_err = err;
                    if attempt + 1 < attempts as usize {
                        let base = self.inner.config.verda.create_backoff_base_seconds;
                        if base > 0.0 {
                            let delay = (base * 2f64.powi(attempt as i32)).min(base * 8.0);
                            if self
                                .cancelled_or_sleep(Duration::from_secs_f64(delay))
                                .await
                            {
                                tracing::info!("verda_create_aborted_shutdown");
                                return Ok(None);
                            }
                        }
                    }
                }
            }
        }
        Err(VerdaError::Message(format!(
            "Verda spot create failed after {attempts} candidate(s): {last_err}"
        )))
    }

    async fn create_one(
        &self,
        choice: &SpotChoice,
        instance_types: &[crate::types::InstanceType],
        images: &[crate::types::Image],
        private_key_file: Option<&str>,
    ) -> Result<Instance, VerdaError> {
        if self.shutdown_cancelled() {
            return Err(VerdaError::Message("shutdown".into()));
        }
        if self
            .inner
            .client
            .confirm_availability(&choice.instance_type)
            .await
            == Some(false)
        {
            return Err(VerdaError::Message("no_capacity".into()));
        }
        let type_info = instance_types
            .iter()
            .find(|t| t.instance_type == choice.instance_type);
        let image = pick_ubuntu24_nvidia_docker_image(images, &self.inner.config.verda, type_info)
            .ok_or_else(|| {
                VerdaError::Message(
                    "no Ubuntu 24 image available for the selected instance type".into(),
                )
            })?;
        if self.shutdown_cancelled() {
            return Err(VerdaError::Message("shutdown".into()));
        }
        let ssh_key_id = ensure_ssh_key_id(
            &self.inner.client,
            &self.inner.config.verda,
            private_key_file,
        )
        .await?;
        let startup_script_id = self.ensure_startup_script_id().await?;
        if self.shutdown_cancelled() {
            return Err(VerdaError::Message("shutdown".into()));
        }
        let hostname = hostname_for(&self.router_id());
        let mut payload = json!({
            "instance_type": choice.instance_type,
            "image": image,
            "hostname": hostname,
            "description": DESCRIPTION,
            "location_code": choice.location_code,
            "is_spot": true,
            "contract": "SPOT",
            "startup_script_id": startup_script_id,
            "os_volume": {
                "name": "ollama-router-os",
                "size": self.inner.config.verda.os_volume_gb,
                "on_spot_discontinue": self.inner.config.verda.on_spot_discontinue,
            },
            "tags": self.managed_tags(),
        });
        if let Some(id) = ssh_key_id {
            payload["ssh_key_ids"] = json!([id]);
        }
        let created = self.inner.client.create_instance(payload).await?;
        let instance_id = created
            .instance_id_value()
            .ok_or_else(|| VerdaError::Message("Verda create returned no instance id".into()))?
            .to_string();
        tracing::info!(
            instance_type = %choice.instance_type,
            location_code = %choice.location_code,
            vram_gb = choice.gpu_memory_gb,
            "verda_created"
        );
        match self.wait_running(&instance_id).await {
            Ok(mut inst) => {
                if inst
                    .hostname
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_none()
                {
                    inst.hostname = Some(hostname);
                }
                self.emit("create");
                Ok(inst)
            }
            Err(err) => {
                let _ = self.compensate(&created).await;
                Err(err)
            }
        }
    }

    async fn ensure_startup_script_id(&self) -> Result<String, VerdaError> {
        let cfg = &self.inner.config.verda;
        if let Some(id) = cfg
            .startup_script_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(id.to_string());
        }
        {
            let cached = self.inner.startup_script_id.lock().await;
            if let Some(id) = cached.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                return Ok(id.to_string());
            }
        }
        let name = cfg.startup_script_name.trim();
        let listed = self.inner.client.list_startup_scripts().await?;
        if let Some(id) = listed.iter().find_map(|script| {
            let listed_name = script.name.as_deref().map(str::trim)?;
            if listed_name == name {
                script.script_key().map(str::to_string)
            } else {
                None
            }
        }) {
            *self.inner.startup_script_id.lock().await = Some(id.clone());
            return Ok(id);
        }
        let body = self.build_startup_script()?;
        let created = self.inner.client.create_startup_script(name, &body).await?;
        let id = created.script_key().map(str::to_string).ok_or_else(|| {
            VerdaError::Message("Verda create startup script returned no id".into())
        })?;
        *self.inner.startup_script_id.lock().await = Some(id.clone());
        Ok(id)
    }

    fn build_startup_script(&self) -> Result<String, VerdaError> {
        let cfg = &self.inner.config.verda;
        let env = OsEnv;
        let (deb_amd64, deb_arm64, tar_amd64, tar_arm64) = default_package_urls(cfg);
        let zrok = env
            .var(&cfg.zrok_enable_token_env)
            .filter(|s| !s.trim().is_empty());
        let enroll_token = env
            .var(&cfg.enroll_token_env)
            .filter(|s| !s.trim().is_empty());
        let params = StartupScriptParams {
            enroll_url: cfg
                .enroll_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            zrok_enable_token: zrok.as_deref(),
            zrok_api_endpoint: self.inner.config.tunnel.api_endpoint(),
            enroll_token: enroll_token.as_deref(),
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
        };
        agent_init_script(&params).map_err(VerdaError::Message)
    }

    async fn wait_running(&self, instance_id: &str) -> Result<Instance, VerdaError> {
        let timeout = Duration::from_secs_f64(self.inner.config.verda.create_timeout_seconds);
        let poll = Duration::from_secs_f64(self.inner.config.verda.poll_interval_seconds.max(0.5));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let instance = self.inner.client.get_instance(instance_id).await?;
            let status = instance
                .status
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            if TERMINAL.contains(&status.as_str()) {
                return Err(VerdaError::Message(format!(
                    "Verda instance {instance_id} reached terminal status {status}"
                )));
            }
            if EVICTED.contains(&status.as_str()) {
                return Err(VerdaError::Message(
                    "no_capacity: spot evicted during create".into(),
                ));
            }
            if status == "running" {
                tracing::info!(instance_id, "verda_running");
                return Ok(instance);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(VerdaError::Message(format!(
                    "Verda instance {instance_id} not running within {:.0}s (status={status})",
                    timeout.as_secs_f64()
                )));
            }
            if self.cancelled_or_sleep(poll).await {
                return Err(VerdaError::Message("shutdown".into()));
            }
        }
    }

    async fn compensate(&self, instance: &Instance) -> Result<(), VerdaError> {
        let Some(id) = instance.instance_id_value() else {
            return Ok(());
        };
        match self
            .inner
            .client
            .delete_instance(id, instance.os_volume_id.clone().map(|v| vec![v]), true)
            .await
        {
            Ok(_) => Ok(()),
            Err(err) => {
                tracing::error!(instance_id = id, error = %err, "verda_create_compensation_failed");
                Err(err)
            }
        }
    }

    pub async fn destroy_all_owned(&self) -> Value {
        let _lock = self.inner.ensure_lock.lock().await;
        let timeout = Duration::from_secs_f64(self.inner.config.verda.destroy_timeout_seconds);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut ids: BTreeMap<String, Option<String>> = BTreeMap::new();
        let nodes = match self.inner.fleet_state.load_async().await {
            Ok(all) => all
                .into_iter()
                .filter(|(_, e)| e.managed_by.as_deref() == Some("verda"))
                .collect(),
            Err(_) => self.inner.fleet_state.snapshot_verda_nodes(),
        };
        for entry in nodes.values() {
            if let Some(id) = entry.verda_instance_id.clone() {
                ids.insert(id, entry.verda_os_volume_id.clone());
            }
        }
        if let Ok(live) = self.inner.client.list_instances().await {
            for inst in live.into_iter().filter(|i| self.is_owned(i)) {
                if let Some(id) = inst.instance_id_value() {
                    ids.entry(id.to_string())
                        .or_insert(inst.os_volume_id.clone());
                }
            }
        }
        let mut deleted = Vec::new();
        let mut failed = Vec::new();
        for (instance_id, volume) in ids {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(instance_id = %instance_id, "verda_destroy_timeout");
                failed.push(instance_id);
                continue;
            }
            match self
                .inner
                .client
                .delete_instance(&instance_id, volume.map(|v| vec![v]), true)
                .await
            {
                Ok(_) => {
                    tracing::info!(instance_id = %instance_id, "verda_destroyed");
                    deleted.push(instance_id.clone());
                    self.forget_instance(&instance_id).await;
                    self.emit("destroy");
                }
                Err(err) => {
                    tracing::error!(instance_id = %instance_id, error = %err, "verda_destroy_failed");
                    failed.push(instance_id);
                }
            }
        }
        json!({"deleted": deleted, "failed": failed})
    }

    pub async fn status(&self) -> Value {
        let instances = match self.inner.client.list_instances().await {
            Ok(list) => list
                .into_iter()
                .filter(|i| self.is_owned(i))
                .collect::<Vec<_>>(),
            Err(err) => {
                return json!({"error": err.to_string().chars().take(200).collect::<String>(), "instances": []});
            }
        };
        let prices: BTreeMap<String, Option<f64>> = self
            .inner
            .client
            .get_instance_types()
            .await
            .ok()
            .map(|types| {
                types
                    .into_iter()
                    .map(|t| (t.instance_type.clone(), t.spot_price_float()))
                    .collect()
            })
            .unwrap_or_default();
        let fleet = self.inner.fleet_state.snapshot_verda_nodes();
        let out: Vec<Value> = instances
            .iter()
            .map(|instance| {
                let instance_id = instance.instance_id_value().unwrap_or("unknown");
                let node_id = Self::node_id_for(instance_id);
                let entry = fleet.get(node_id.as_str());
                json!({
                    "node_id": node_id.as_str(),
                    "instance_id": instance_id,
                    "instance_type": instance.instance_type,
                    "location_code": instance.location_value(),
                    "spot_price": instance.instance_type.as_ref().and_then(|t| prices.get(t).copied().flatten()),
                    "status": instance.status,
                    "ip": instance.public_ip_value(),
                    "url": entry.and_then(|e| e.url.clone()),
                    "local_access_url": entry.and_then(|e| e.local_access_url.clone()),
                    "mode": "enroll",
                    "in_registry": self.inner.registry.get(&node_id).is_some(),
                })
            })
            .collect();
        json!({"enabled": self.inner.config.verda.enabled, "instances": out})
    }

    /// Snapshot the latest demand recovery without exposing provider secrets.
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

    pub async fn reconcile(&self) {
        if self.shutdown_cancelled() {
            return;
        }
        let live = match self.inner.client.list_instances().await {
            Ok(v) => v,
            Err(err) => {
                tracing::error!(error = %err, "verda_reconcile_list_failed");
                return;
            }
        };
        let owned: Vec<_> = live.into_iter().filter(|i| self.is_owned(i)).collect();
        let live_ids: HashSet<String> = owned
            .iter()
            .filter_map(|i| i.instance_id_value().map(str::to_string))
            .collect();
        let fleet = self.inner.fleet_state.snapshot_verda_nodes();
        for (node_id, entry) in fleet {
            let Some(iid) = entry.verda_instance_id.clone() else {
                continue;
            };
            if !live_ids.contains(&iid) {
                tracing::info!(node_id = %node_id, instance_id = %iid, "verda_reconcile_gone");
                if let Ok(nid) = NodeId::parse(&node_id) {
                    self.inner.registry.remove_verda(&nid);
                }
                if let Err(err) = self.inner.fleet_state.remove_async(&node_id).await {
                    tracing::warn!(node_id = %node_id, error = %err, "verda reconcile remove failed");
                }
                continue;
            }
            if let Some(inst) = owned
                .iter()
                .find(|i| i.instance_id_value() == Some(iid.as_str()))
            {
                let status = inst.status.as_deref().unwrap_or("").to_ascii_lowercase();
                if TERMINAL.contains(&status.as_str()) || EVICTED.contains(&status.as_str()) {
                    self.forget_instance(&iid).await;
                }
            }
        }
        for inst in &owned {
            if self.shutdown_cancelled() {
                return;
            }
            let status = inst.status.as_deref().unwrap_or("").to_ascii_lowercase();
            if status != "running" {
                continue;
            }
            let Some(iid) = inst.instance_id_value() else {
                continue;
            };
            let node_id = Self::node_id_for(iid);
            if self.inner.registry.get(&node_id).is_none() {
                let _ = self.adopt(inst).await;
            }
        }
        if self.shutdown_cancelled() || !self.inner.config.verda.auto_scale {
            return;
        }
        let min_n = self.inner.config.verda.auto_scale_min_instances;
        let max_n = self.inner.config.verda.auto_scale_max_instances;
        let in_flight: Vec<_> = owned
            .iter()
            .filter(|i| {
                let s = i.status.as_deref().unwrap_or("").to_ascii_lowercase();
                !TERMINAL.contains(&s.as_str()) && !EVICTED.contains(&s.as_str())
            })
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
                    tracing::error!(error = %err, "verda_reconcile_scale_up_failed");
                    break;
                }
            }
        }
        if self.shutdown_cancelled() {
            return;
        }
        if self.inner.config.verda.idle_scale_down_enabled {
            self.idle_scale_down(&in_flight, min_n).await;
        }
        if self.shutdown_cancelled() {
            return;
        }
        let remaining: Vec<_> = in_flight
            .iter()
            .filter(|i| {
                i.instance_id_value()
                    .is_some_and(|id| self.inner.registry.get(&Self::node_id_for(id)).is_some())
            })
            .cloned()
            .collect();
        if max_n > 0 && remaining.len() as u32 > max_n {
            let excess = remaining.len() as u32 - max_n;
            for inst in remaining.iter().take(excess as usize) {
                if let Some(id) = inst.instance_id_value() {
                    if self
                        .inner
                        .client
                        .delete_instance(id, inst.os_volume_id.clone().map(|v| vec![v]), true)
                        .await
                        .is_ok()
                    {
                        self.forget_instance(id).await;
                    }
                }
            }
        }
    }

    async fn idle_scale_down(&self, in_flight: &[Instance], min_n: u32) {
        let now = std::time::Instant::now();
        let views: Vec<IdleNodeView> = in_flight
            .iter()
            .filter_map(|inst| {
                let iid = inst.instance_id_value()?;
                let node_id = Self::node_id_for(iid);
                let snap = self.inner.registry.get(&node_id)?;
                let instance_id = VerdaInstanceId::parse(iid).ok()?;
                let registered = self.inner.registry.registered_at(&node_id).unwrap_or(now);
                let last = self.inner.registry.last_client_request_at(&node_id);
                Some(IdleNodeView {
                    node_id,
                    instance_id,
                    origin: snap.origin,
                    inflight: snap.inflight,
                    registered_at: registered,
                    last_client_request_at: last,
                })
            })
            .collect();
        let policy = IdlePolicy {
            idle_timeout: Duration::from_secs_f64(
                self.inner.config.verda.idle_timeout_seconds.max(0.0),
            ),
            grace_after_create: Duration::from_secs_f64(
                self.inner
                    .config
                    .verda
                    .idle_grace_after_create_seconds
                    .max(0.0),
            ),
            min_instances: min_n,
        };
        for cand in idle_scale_down_candidates(&views, now, policy) {
            let inst = in_flight
                .iter()
                .find(|i| i.instance_id_value() == Some(cand.instance_id.as_str()));
            let Some(inst) = inst else {
                continue;
            };
            match self
                .inner
                .client
                .delete_instance(
                    cand.instance_id.as_str(),
                    inst.os_volume_id.clone().map(|v| vec![v]),
                    true,
                )
                .await
            {
                Ok(_) => {
                    tracing::info!(
                        node_id = %cand.node_id,
                        instance_id = %cand.instance_id,
                        "verda_idle_scale_down"
                    );
                    self.forget_instance(cand.instance_id.as_str()).await;
                    self.emit("idle");
                }
                Err(err) => {
                    tracing::error!(
                        node_id = %cand.node_id,
                        instance_id = %cand.instance_id,
                        error = %err,
                        "verda_scale_down_destroy_failed"
                    );
                }
            }
        }
    }

    pub async fn run_reconcile_loop(self) {
        let interval =
            Duration::from_secs_f64(self.inner.config.verda.poll_interval_seconds.max(1.0));
        loop {
            if self.shutdown_cancelled() {
                tracing::info!("verda_reconcile_loop_stopped");
                return;
            }
            self.reconcile().await;
            if self.cancelled_or_sleep(interval).await {
                tracing::info!("verda_reconcile_loop_stopped");
                return;
            }
        }
    }
}

impl DemandScale for VerdaManager {
    fn request_scale_up(&self, reason: RoutingError) {
        let reason_code = reason.as_reason_code();
        if self.shutdown_cancelled() {
            tracing::info!(reason = reason_code, "verda_demand_scale_up_skipped");
            return;
        }
        if !self.inner.config.verda.auto_scale {
            tracing::info!(reason = reason_code, "verda_demand_scale_up_skipped");
            return;
        }
        let max_n = self.inner.config.verda.auto_scale_max_instances;
        if max_n > 0 {
            let owned = self
                .inner
                .registry
                .snapshot()
                .iter()
                .filter(|n| n.origin == NodeOrigin::Verda)
                .count() as u32;
            if owned >= max_n {
                tracing::info!(
                    reason = reason_code,
                    owned_count = owned,
                    max_n,
                    "verda_demand_scale_up_capped"
                );
                return;
            }
        }
        let mut slot = self.lock_demand();
        if self.shutdown_cancelled() {
            return;
        }
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            tracing::info!(reason = reason_code, "verda_demand_scale_up_coalesced");
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
        self.emit("demand");
        *slot = Some(tokio::spawn(async move {
            let mgr = VerdaManager { inner };
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
            tracing::info!(reason = reason_code, "verda_demand_scale_up");
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
                    tracing::error!(reason = reason_code, error = %err, "verda_demand_ensure_failed");
                }
                Err(_) => {}
            }
        }));
    }
}

impl Clone for VerdaManager {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

fn hostname_for(router_id: &str) -> String {
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
    let take: String = slug.chars().take(8).collect();
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(seq);
    format!("or-{take}-{nanos:x}{seq:x}")
}

fn allowlisted_url_reason(err: &str) -> &'static str {
    if err.contains(REASON_PUBLIC_URL_BLOCKED) {
        REASON_PUBLIC_URL_BLOCKED
    } else {
        REASON_TAGS_UNREACHABLE
    }
}
