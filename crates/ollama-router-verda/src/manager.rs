//! Verda spot fleet manager: create, adopt, destroy, reconcile, idle scale-down.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ollama_router_core::cloud::{
    idle_scale_down_candidates, DemandScale, FleetEvents, IdleNodeView, IdlePolicy,
};
use ollama_router_core::config::{Capacity, NodeConfig, NodeSshConfig, OsEnv, RouterConfig};
use ollama_router_core::fleet::{
    FleetState, NodeId, NodeOrigin, Registry, VerdaInstanceId, VerdaNodePersist,
};
use ollama_router_core::provision::{
    provision_config_from_defaults, NodeProvisioner, ProvisionOpts, ProvisionStatus,
};
use ollama_router_core::routing::RoutingError;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::client::{VerdaClient, VerdaError};
use crate::images::pick_ubuntu24_nvidia_docker_image;
use crate::keys::{companion_private_key, ensure_ssh_key_id};
use crate::selector::{rank_candidates, SpotChoice};
use crate::types::Instance;

pub const MANAGED_BY: &str = "ollama-router";
const DESCRIPTION: &str = "ollama-router managed spot";
const TERMINAL: &[&str] = &["failed", "error", "deleted", "terminated"];
const EVICTED: &[&str] = &["discontinued", "evicted"];

pub struct VerdaManager {
    inner: Arc<Inner>,
}

struct Inner {
    config: Arc<RouterConfig>,
    client: VerdaClient,
    registry: Arc<Registry>,
    fleet_state: Arc<FleetState>,
    provisioner: Arc<dyn NodeProvisioner>,
    ensure_lock: Mutex<()>,
    demand: std::sync::Mutex<Option<JoinHandle<()>>>,
    events: std::sync::Mutex<Option<Arc<dyn FleetEvents>>>,
}

impl VerdaManager {
    pub fn new(
        config: Arc<RouterConfig>,
        client: VerdaClient,
        registry: Arc<Registry>,
        fleet_state: Arc<FleetState>,
        provisioner: Arc<dyn NodeProvisioner>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                client,
                registry,
                fleet_state,
                provisioner,
                ensure_lock: Mutex::new(()),
                demand: std::sync::Mutex::new(None),
                events: std::sync::Mutex::new(None),
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

    pub fn node_id_for(instance_id: &str) -> NodeId {
        let raw = format!("verda-{instance_id}");
        NodeId::parse(&raw).unwrap_or_else(|_| {
            NodeId::parse("verda-unknown").unwrap_or_else(|_| unreachable!("static node id"))
        })
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
        if let Some(p) = self.inner.config.verda.ssh_public_key_file.as_deref() {
            return Some(
                companion_private_key(std::path::Path::new(p))
                    .to_string_lossy()
                    .into(),
            );
        }
        self.inner
            .config
            .nodes
            .iter()
            .find_map(|n| n.ssh.as_ref().and_then(|s| s.key_file.clone()))
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_verda(
        &self,
        node_id: &NodeId,
        url: &str,
        instance: &Instance,
        location: &str,
        instance_type: &str,
        tailscale_ip: Option<&str>,
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
            tailscale_ip,
            spot_price_per_hour: spot_price,
        };
        if let Err(err) = self
            .inner
            .fleet_state
            .persist_verda_node(node_id.as_str(), persist)
        {
            tracing::error!(node_id = %node_id, error = %err, "verda persist failed");
        }
    }

    fn build_node(
        &self,
        instance: &Instance,
        choice: &SpotChoice,
        private_key_file: Option<&str>,
        url: Option<String>,
    ) -> Option<NodeConfig> {
        let instance_id = instance.instance_id_value()?;
        let ip = instance.public_ip_value().unwrap_or("");
        let mut prov = provision_config_from_defaults(&self.inner.config.provision_defaults);
        if let Some(accept) = self.inner.config.verda.ts_accept_routes {
            prov.ts_accept_routes = accept;
        }
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
            ssh: Some(NodeSshConfig {
                host: ip.to_string(),
                port: 22,
                user: "root".into(),
                key_file: private_key_file.map(str::to_string),
                password_env: None,
            }),
            provision: Some(prov),
        })
    }

    fn forget_instance(&self, instance_id: &str) {
        let node_id = Self::node_id_for(instance_id);
        self.inner.registry.remove_verda(&node_id);
        let _ = self.inner.fleet_state.remove(node_id.as_str());
    }

    pub async fn ensure(&self, create: bool) -> Result<Value, VerdaError> {
        let _lock = self.inner.ensure_lock.lock().await;
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
                    self.forget_instance(id);
                }
                continue;
            }
            if status == "running" && instance.public_ip_value().is_some() {
                return Ok(self.adopt(instance).await);
            }
        }
        if !create {
            return Ok(json!({"status": "none"}));
        }
        let key = self.private_key_file();
        if key.is_none() && self.inner.config.verda.ssh_public_key_file.is_none() {
            return Err(VerdaError::Message(
                "Verda ensure needs a local SSH key: set verda.ssh_private_key_file".into(),
            ));
        }
        let Some((created, choice)) = self.create_instance(key.as_deref()).await? else {
            return Ok(json!({"status": "none"}));
        };
        Ok(self.provision_new(&created, &choice, key.as_deref()).await)
    }

    pub async fn create_additional(&self) -> Result<Value, VerdaError> {
        let _lock = self.inner.ensure_lock.lock().await;
        self.create_additional_locked()
            .await
            .inspect_err(|_| self.emit("ensure_failed"))
    }

    async fn create_additional_locked(&self) -> Result<Value, VerdaError> {
        let key = self.private_key_file();
        if key.is_none() && self.inner.config.verda.ssh_public_key_file.is_none() {
            return Err(VerdaError::Message(
                "Verda ensure needs a local SSH key: set verda.ssh_private_key_file".into(),
            ));
        }
        let Some((created, choice)) = self.create_instance(key.as_deref()).await? else {
            return Ok(json!({"status": "none"}));
        };
        let result = self.provision_new(&created, &choice, key.as_deref()).await;
        if result.get("provision").and_then(Value::as_str) == Some("fail") {
            let _ = self.compensate(&created).await;
        }
        Ok(result)
    }

    async fn adopt(&self, instance: &Instance) -> Value {
        let instance_id = instance.instance_id_value().unwrap_or("unknown");
        let instance_type = instance.instance_type.clone().unwrap_or_default();
        let location = instance.location_value().unwrap_or("").to_string();
        let node_id = Self::node_id_for(instance_id);
        tracing::info!(
            instance_id,
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
        let Some(mut node) =
            self.build_node(instance, &choice, self.private_key_file().as_deref(), None)
        else {
            return json!({"status": "none"});
        };
        if let Ok(Some(url)) = self.inner.fleet_state.hydrate_url(&node_id) {
            node.url = Some(url.clone());
            self.inner.registry.upsert_verda(node.clone());
            let _ = self.inner.registry.set_node_url(&node_id, &url);
            self.persist_verda(
                &node_id,
                &url,
                instance,
                &location,
                &instance_type,
                None,
                None,
            );
            self.emit("adopt");
            return json!({
                "status": "adopted",
                "node_id": node_id.as_str(),
                "instance_id": instance_id,
                "provision": "hydrated",
                "url": url,
            });
        }
        node.url = None;
        self.inner.registry.upsert_verda(node.clone());
        self.persist_verda(
            &node_id,
            "",
            instance,
            &location,
            &instance_type,
            None,
            None,
        );
        let result = self
            .inner
            .provisioner
            .provision_node(node, ProvisionOpts::default())
            .await;
        let url = self
            .inner
            .registry
            .node_config(&node_id)
            .and_then(|n| n.url);
        self.persist_verda(
            &node_id,
            url.as_deref().unwrap_or(""),
            instance,
            &location,
            &instance_type,
            result.tailscale_ip.as_deref(),
            None,
        );
        self.emit("adopt");
        json!({
            "status": "adopted",
            "node_id": node_id.as_str(),
            "instance_id": instance_id,
            "instance_type": instance_type,
            "location_code": location,
            "url": url,
            "provision": result.status.as_str(),
            "tailscale_ip": result.tailscale_ip,
            "detail": result.detail,
            "phase": result.phase,
        })
    }

    async fn provision_new(
        &self,
        instance: &Instance,
        choice: &SpotChoice,
        private_key_file: Option<&str>,
    ) -> Value {
        let instance_id = instance.instance_id_value().unwrap_or("unknown");
        let node_id = Self::node_id_for(instance_id);
        let Some(node) = self.build_node(instance, choice, private_key_file, None) else {
            return json!({"status": "none"});
        };
        self.inner.registry.upsert_verda(node.clone());
        self.persist_verda(
            &node_id,
            "",
            instance,
            &choice.location_code,
            &choice.instance_type,
            None,
            Some(choice.spot_price),
        );
        let result = self
            .inner
            .provisioner
            .provision_node(
                node,
                ProvisionOpts {
                    wait_for_public_ssh: true,
                    ..ProvisionOpts::default()
                },
            )
            .await;
        let url = self
            .inner
            .registry
            .node_config(&node_id)
            .and_then(|n| n.url);
        if result.status == ProvisionStatus::Ok {
            self.persist_verda(
                &node_id,
                url.as_deref().unwrap_or(""),
                instance,
                &choice.location_code,
                &choice.instance_type,
                result.tailscale_ip.as_deref(),
                Some(choice.spot_price),
            );
        }
        json!({
            "status": "created",
            "node_id": node_id.as_str(),
            "instance_id": instance_id,
            "instance_type": choice.instance_type,
            "location_code": choice.location_code,
            "url": url,
            "provision": result.status.as_str(),
            "tailscale_ip": result.tailscale_ip,
            "detail": result.detail,
            "phase": result.phase,
        })
    }

    async fn create_instance(
        &self,
        private_key_file: Option<&str>,
    ) -> Result<Option<(Instance, SpotChoice)>, VerdaError> {
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
                            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
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
        let ssh_key_id = ensure_ssh_key_id(
            &self.inner.client,
            &self.inner.config.verda,
            private_key_file,
        )
        .await?;
        let hostname = hostname_for(&self.router_id());
        let payload = json!({
            "instance_type": choice.instance_type,
            "image": image,
            "ssh_key_ids": [ssh_key_id],
            "hostname": hostname,
            "description": DESCRIPTION,
            "location_code": choice.location_code,
            "is_spot": true,
            "contract": "SPOT",
            "os_volume": {
                "name": "ollama-router-os",
                "size": self.inner.config.verda.os_volume_gb,
                "on_spot_discontinue": self.inner.config.verda.on_spot_discontinue,
            },
            "tags": self.managed_tags(),
        });
        let created = self.inner.client.create_instance(payload).await?;
        let instance_id = created
            .instance_id_value()
            .ok_or_else(|| VerdaError::Message("Verda create returned no instance id".into()))?
            .to_string();
        tracing::info!(
            instance_id = %instance_id,
            instance_type = %choice.instance_type,
            location_code = %choice.location_code,
            image = %image,
            hostname = %hostname,
            "verda_created"
        );
        match self.wait_running(&instance_id).await {
            Ok(inst) => {
                self.emit("create");
                Ok(inst)
            }
            Err(err) => {
                let _ = self.compensate(&created).await;
                Err(err)
            }
        }
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
            if status == "running" && instance.public_ip_value().is_some() {
                tracing::info!(
                    instance_id,
                    ip = instance.public_ip_value().unwrap_or(""),
                    "verda_running"
                );
                return Ok(instance);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(VerdaError::Message(format!(
                    "Verda instance {instance_id} not running within {:.0}s (status={status})",
                    timeout.as_secs_f64()
                )));
            }
            tokio::time::sleep(poll).await;
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
        if let Ok(nodes) = self.inner.fleet_state.list_verda_nodes() {
            for entry in nodes.values() {
                if let Some(id) = entry.verda_instance_id.clone() {
                    ids.insert(id, entry.verda_os_volume_id.clone());
                }
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
                    self.forget_instance(&instance_id);
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
        let fleet = self
            .inner
            .fleet_state
            .list_verda_nodes()
            .unwrap_or_default();
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
                    "tailscale_ip": entry.and_then(|e| e.tailscale_ip.clone()),
                    "mode": "ssh",
                    "in_registry": self.inner.registry.get(&node_id).is_some(),
                })
            })
            .collect();
        json!({"enabled": self.inner.config.verda.enabled, "instances": out})
    }

    pub async fn reconcile(&self) {
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
        if let Ok(fleet) = self.inner.fleet_state.list_verda_nodes() {
            for (node_id, entry) in fleet {
                let Some(iid) = entry.verda_instance_id.clone() else {
                    continue;
                };
                if !live_ids.contains(&iid) {
                    tracing::info!(node_id = %node_id, instance_id = %iid, "verda_reconcile_gone");
                    if let Ok(nid) = NodeId::parse(&node_id) {
                        self.inner.registry.remove_verda(&nid);
                    }
                    let _ = self.inner.fleet_state.remove(&node_id);
                    continue;
                }
                if let Some(inst) = owned
                    .iter()
                    .find(|i| i.instance_id_value() == Some(iid.as_str()))
                {
                    let status = inst.status.as_deref().unwrap_or("").to_ascii_lowercase();
                    if TERMINAL.contains(&status.as_str()) || EVICTED.contains(&status.as_str()) {
                        self.forget_instance(&iid);
                    }
                }
            }
        }
        for inst in &owned {
            let status = inst.status.as_deref().unwrap_or("").to_ascii_lowercase();
            if status != "running" || inst.public_ip_value().is_none() {
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
        if !self.inner.config.verda.auto_scale {
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
            match self.create_additional().await {
                Ok(_) => count += 1,
                Err(err) => {
                    tracing::error!(error = %err, "verda_reconcile_scale_up_failed");
                    break;
                }
            }
        }
        if self.inner.config.verda.idle_scale_down_enabled {
            self.idle_scale_down(&in_flight, min_n).await;
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
                        self.forget_instance(id);
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
                    self.forget_instance(cand.instance_id.as_str());
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
            self.reconcile().await;
            tokio::time::sleep(interval).await;
        }
    }
}

impl DemandScale for VerdaManager {
    fn request_scale_up(&self, reason: RoutingError) {
        let reason_code = reason.as_reason_code();
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
        let mut slot = match self.inner.demand.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            tracing::info!(reason = reason_code, "verda_demand_scale_up_coalesced");
            return;
        }
        let inner = self.inner.clone();
        self.emit("demand");
        *slot = Some(tokio::spawn(async move {
            let mgr = VerdaManager { inner };
            tracing::info!(reason = reason_code, "verda_demand_scale_up");
            if let Err(err) = mgr.create_additional().await {
                tracing::error!(reason = reason_code, error = %err, "verda_demand_ensure_failed");
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
    let take: String = slug.chars().take(16).collect();
    format!("orouter-{take}")
}
