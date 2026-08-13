//! Two-phase SSH provision: public bootstrap → ordinary OpenSSH on Tailscale → verify.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ollama_router_core::config::{NodeConfig, NodeSshConfig, RouterConfig};
use ollama_router_core::fleet::{
    is_tailscale_ipv4, ollama_url_for_tailscale_ip, url_host_is_tailscale, FleetState, Registry,
};
use ollama_router_core::provision::{
    posix_quote, read_provision_script, redact_authkey, NodeProvisioner, ProvisionFuture,
    ProvisionOpts, ProvisionPhase, ProvisionResult, ProvisionStatus, REMOTE_SCRIPT,
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

use super::ssh::{RemoteOutput, RusshTransport};

/// SSH endpoint used by the transport (never log passwords/keys).
#[derive(Clone, Debug)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_file: Option<String>,
    pub password: Option<String>,
}

impl SshTarget {
    pub(crate) fn from_node(node: &NodeConfig) -> Option<Self> {
        let ssh = node.ssh.as_ref()?;
        Some(Self::from_ssh(ssh))
    }

    fn from_ssh(ssh: &NodeSshConfig) -> Self {
        let password = ssh
            .password_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            host: ssh.host.clone(),
            port: ssh.port,
            user: ssh.user.clone(),
            key_file: ssh.key_file.clone(),
            password,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// Fan-out SSH provisioner with per-node locks and global concurrency.
pub struct ProvisionOrchestrator {
    config: Arc<RouterConfig>,
    registry: Option<Arc<Registry>>,
    fleet_state: Option<Arc<FleetState>>,
    client: reqwest::Client,
    ssh: RusshTransport,
    sem: Arc<Semaphore>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    inflight: Mutex<HashSet<String>>,
    cooldown_until: Mutex<HashMap<String, Instant>>,
    phase: Mutex<HashMap<String, String>>,
    /// Test double: when set, `probe`/`run` use this instead of russh.
    mock: Option<Arc<dyn MockTransport>>,
}

/// Injected SSH for tests (no live sshd).
pub trait MockTransport: Send + Sync {
    fn probe(
        &self,
        target: &SshTarget,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>>;
    fn run(
        &self,
        target: &SshTarget,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RemoteOutput, String>> + Send + '_>,
    >;
}

impl ProvisionOrchestrator {
    pub fn new(
        config: Arc<RouterConfig>,
        client: reqwest::Client,
        registry: Option<Arc<Registry>>,
        fleet_state: Option<Arc<FleetState>>,
    ) -> Self {
        let concurrency = config.provision_defaults.concurrency.max(1) as usize;
        Self {
            sem: Arc::new(Semaphore::new(concurrency)),
            config,
            registry,
            fleet_state,
            client,
            ssh: RusshTransport,
            locks: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashSet::new()),
            cooldown_until: Mutex::new(HashMap::new()),
            phase: Mutex::new(HashMap::new()),
            mock: None,
        }
    }

    #[cfg(test)]
    pub fn with_mock(mut self, mock: Arc<dyn MockTransport>) -> Self {
        self.mock = Some(mock);
        self
    }

    pub fn auto_enabled(&self) -> bool {
        self.config.provision_defaults.auto
    }

    pub fn is_inflight(&self, node_id: &str) -> bool {
        self.inflight
            .try_lock()
            .map(|g| g.contains(node_id))
            .unwrap_or(true)
    }

    pub fn on_cooldown(&self, node_id: &str) -> bool {
        let now = Instant::now();
        self.cooldown_until
            .try_lock()
            .map(|g| g.get(node_id).is_some_and(|until| now < *until))
            .unwrap_or(false)
    }

    pub fn mark_cooldown(&self, node_id: &str) {
        let until = Instant::now()
            + Duration::from_secs_f64(self.config.provision_defaults.cooldown_seconds);
        if let Ok(mut g) = self.cooldown_until.try_lock() {
            g.insert(node_id.to_string(), until);
        }
    }

    pub fn clear_cooldown(&self, node_id: &str) {
        if let Ok(mut g) = self.cooldown_until.try_lock() {
            g.remove(node_id);
        }
    }

    pub fn current_phase(&self, node_id: &str) -> Option<String> {
        self.phase
            .try_lock()
            .ok()
            .and_then(|g| g.get(node_id).cloned())
    }

    pub fn provisionable_nodes(
        &self,
        node_ids: Option<&[String]>,
    ) -> Result<Vec<NodeConfig>, String> {
        let wanted: Option<HashSet<&str>> =
            node_ids.map(|ids| ids.iter().map(String::as_str).collect());
        if let Some(ids) = node_ids {
            let known: HashSet<&str> = self.config.nodes.iter().map(|n| n.id.as_str()).collect();
            for id in ids {
                if !known.contains(id.as_str()) {
                    return Err(format!("unknown node id: {id}"));
                }
                let Some(node) = self.config.nodes.iter().find(|n| n.id.as_str() == id) else {
                    return Err(format!("unknown node id: {id}"));
                };
                if !node.provision_enabled() {
                    return Err(format!("node {id} has no ssh/provision.enabled"));
                }
            }
        }
        Ok(self
            .config
            .nodes
            .iter()
            .filter(|n| wanted.as_ref().is_none_or(|w| w.contains(n.id.as_str())))
            .filter(|n| n.provision_enabled())
            .cloned()
            .collect())
    }

    pub async fn provision_many(
        &self,
        node_ids: Option<&[String]>,
        opts: ProvisionOpts,
    ) -> Result<Vec<ProvisionResult>, String> {
        let nodes = self.provisionable_nodes(node_ids)?;
        let mut out = Vec::with_capacity(nodes.len());
        for node in nodes {
            out.push(self.provision_node_inner(node, opts).await);
        }
        Ok(out)
    }

    pub async fn provision_node_inner(
        &self,
        node: NodeConfig,
        opts: ProvisionOpts,
    ) -> ProvisionResult {
        if !node.provision_enabled() || node.ssh.is_none() {
            return ProvisionResult::skip(
                node.id.clone(),
                "ssh/provision not enabled",
                ProvisionPhase::Skip,
            );
        }
        let lock = {
            let mut locks = self.locks.lock().await;
            locks
                .entry(node.id.as_str().to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _node_guard = lock.lock().await;
        {
            let inflight = self.inflight.lock().await;
            if inflight.contains(node.id.as_str()) {
                return ProvisionResult::skip(
                    node.id.clone(),
                    "already in flight",
                    ProvisionPhase::Skip,
                );
            }
        }
        if !opts.force && self.on_cooldown(node.id.as_str()) {
            return ProvisionResult::skip(
                node.id.clone(),
                "cooldown active",
                ProvisionPhase::Cooldown,
            );
        }
        let _permit = match self.sem.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                return ProvisionResult::fail(
                    node.id.clone(),
                    "provision semaphore closed",
                    ProvisionPhase::Fail,
                    None,
                );
            }
        };
        {
            let mut inflight = self.inflight.lock().await;
            inflight.insert(node.id.as_str().to_string());
        }
        let result = self.provision_locked(node, opts).await;
        {
            let mut inflight = self.inflight.lock().await;
            inflight.remove(result.node_id.as_str());
        }
        result
    }

    async fn set_phase(&self, node: &NodeConfig, phase: ProvisionPhase) -> String {
        let phase_s = phase.as_str().to_string();
        let prev = {
            let mut g = self.phase.lock().await;
            g.insert(node.id.as_str().to_string(), phase_s.clone())
        };
        let ssh = node
            .ssh
            .as_ref()
            .map(|s| format!("{}@{}:{}", s.user, s.host, s.port))
            .unwrap_or_else(|| "(no-ssh)".into());
        tracing::info!(
            node_id = %node.id,
            provision_phase = %phase_s,
            from_phase = prev.as_deref(),
            ssh_endpoint = %ssh,
            "provision_phase"
        );
        phase_s
    }

    fn ssh_timeout(&self) -> Duration {
        Duration::from_secs_f64(
            self.config
                .provision_defaults
                .ssh_connect_timeout_seconds
                .max(0.1),
        )
    }

    fn tailscale_ssh_port(&self) -> u16 {
        self.config.provision_defaults.tailscale_ssh_port
    }

    pub(crate) async fn probe(&self, target: &SshTarget) -> Result<(), String> {
        if let Some(mock) = &self.mock {
            return mock.probe(target).await;
        }
        self.ssh.probe(target, self.ssh_timeout()).await
    }

    async fn run(
        &self,
        target: &SshTarget,
        command: &str,
        stdin: Option<&[u8]>,
    ) -> Result<RemoteOutput, String> {
        if let Some(mock) = &self.mock {
            return mock.run(target, command, stdin).await;
        }
        match self
            .ssh
            .run(target, self.ssh_timeout(), command, stdin)
            .await
        {
            Ok(out) => Ok(out),
            Err(err)
                if err.contains("disconnect")
                    || err.contains("reset")
                    || err.contains("broken") =>
            {
                Ok(RemoteOutput {
                    exit_status: 0,
                    output: format!("REBOOT_REQUIRED=1\n(ssh disconnected: {err})"),
                    disconnected: true,
                })
            }
            Err(err) => Err(err),
        }
    }

    async fn wait_for_ssh(
        &self,
        node: &NodeConfig,
        timeout: Duration,
        initial_sleep: Duration,
        log_event: &str,
    ) -> bool {
        let Some(target) = SshTarget::from_node(node) else {
            return false;
        };
        tracing::info!(
            node_id = %node.id,
            timeout_seconds = timeout.as_secs_f64(),
            ssh_endpoint = %target.endpoint(),
            "{log_event}"
        );
        if !initial_sleep.is_zero() {
            tokio::time::sleep(initial_sleep).await;
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.probe(&target).await.is_ok() {
                tracing::info!(node_id = %node.id, ssh_endpoint = %target.endpoint(), "provision_ssh_back");
                return true;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        false
    }

    fn known_tailscale_ip(&self, node: &NodeConfig) -> Option<String> {
        if let Some(url) = node.url.as_deref() {
            if let Ok(parsed) = Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    if is_tailscale_ipv4(host) {
                        return Some(host.to_string());
                    }
                }
            }
        }
        let fs = self.fleet_state.as_ref()?;
        let entry = fs.get_entry(node.id.as_str()).ok().flatten()?;
        if let Some(ip) = entry.tailscale_ip.as_deref() {
            if is_tailscale_ipv4(ip) {
                return Some(ip.trim().to_string());
            }
        }
        if let Some(url) = entry.url.as_deref() {
            if let Ok(parsed) = Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    if is_tailscale_ipv4(host) {
                        return Some(host.to_string());
                    }
                }
            }
        }
        None
    }

    fn switch_ssh_to_tailscale(&self, node: &mut NodeConfig, ts_ip: &str) {
        let port = self.tailscale_ssh_port();
        let old = node
            .ssh
            .as_ref()
            .map(|s| format!("{}@{}:{}", s.user, s.host, s.port))
            .unwrap_or_default();
        if let Some(ssh) = node.ssh.as_mut() {
            ssh.host = ts_ip.to_string();
            ssh.port = port;
        }
        if let Some(reg) = &self.registry {
            reg.set_ssh_endpoint(&node.id, ts_ip, port);
        }
        tracing::info!(
            node_id = %node.id,
            from_endpoint = %old,
            to_endpoint = %format!("{}@{}:{port}", node.ssh.as_ref().map(|s| s.user.as_str()).unwrap_or(""), ts_ip),
            tailscale_ip = ts_ip,
            "provision_ssh_switched_to_tailscale"
        );
    }

    fn persist_tailscale_meta(&self, node: &NodeConfig, ts_ip: &str, url: &str) {
        let Some(fs) = &self.fleet_state else {
            return;
        };
        if let Err(err) = fs.persist_url(node.id.as_str(), url, Some(ts_ip)) {
            tracing::error!(
                node_id = %node.id,
                tailscale_ip = ts_ip,
                error = %err,
                "provision_tailscale_meta_persist_failed"
            );
        } else {
            tracing::info!(
                node_id = %node.id,
                tailscale_ip = ts_ip,
                url = url,
                "provision_tailscale_meta_persisted"
            );
        }
    }

    fn apply_tailscale_url(&self, node: &mut NodeConfig, ts_ip: &str) -> Result<String, String> {
        let url = ollama_url_for_tailscale_ip(ts_ip)?;
        node.url = Some(url.clone());
        self.persist_tailscale_meta(node, ts_ip, &url);
        if let Some(reg) = &self.registry {
            reg.set_node_url(&node.id, &url)?;
        }
        Ok(url)
    }

    fn fail(
        &self,
        node: &NodeConfig,
        detail: String,
        phase: ProvisionPhase,
        tailscale_ip: Option<String>,
    ) -> ProvisionResult {
        self.mark_cooldown(node.id.as_str());
        if let Some(reg) = &self.registry {
            reg.clear_unsafe_routing_url(&node.id);
        }
        tracing::warn!(
            node_id = %node.id,
            phase = phase.as_str(),
            detail = %detail.chars().take(200).collect::<String>(),
            ssh_endpoint = %node.ssh.as_ref().map(|s| format!("{}@{}:{}", s.user, s.host, s.port)).unwrap_or_default(),
            tailscale_ip = tailscale_ip.as_deref(),
            "provision_unfinished"
        );
        ProvisionResult::fail(node.id.clone(), detail, phase, tailscale_ip)
    }

    fn ts_authkey(&self) -> Option<String> {
        let name = self.config.provision_defaults.ts_authkey_env.trim();
        std::env::var(name)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn remote_env(&self, node: &NodeConfig, phase: &str) -> Vec<(String, String)> {
        let prov = node.provision.as_ref();
        let mut env = vec![
            (
                "TS_HOSTNAME".into(),
                prov.and_then(|p| p.ts_hostname.clone())
                    .unwrap_or_else(|| node.id.as_str().to_string()),
            ),
            (
                "OS_UPGRADE".into(),
                if prov.is_none_or(|p| p.os_upgrade) {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ),
            (
                "SKIP_MODELS".into(),
                if prov.is_some_and(|p| p.skip_models || p.skip_ollama) {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ),
            (
                "SKIP_OLLAMA".into(),
                if prov.is_some_and(|p| p.skip_ollama) {
                    "1"
                } else {
                    "0"
                }
                .into(),
            ),
            ("PROVISION_PHASE".into(), phase.to_string()),
        ];
        if prov.is_some_and(|p| p.ts_ephemeral) {
            env.push(("TS_EPHEMERAL".into(), "1".into()));
        }
        if prov.is_some_and(|p| p.ts_accept_routes) {
            env.push(("TS_ACCEPT_ROUTES".into(), "1".into()));
        }
        if let Some(tags) = prov.and_then(|p| p.ts_tags.clone()) {
            env.push(("TS_TAGS".into(), tags));
        }
        if let Some(routes) = prov.and_then(|p| p.ts_advertise_routes.clone()) {
            env.push(("TS_ADVERTISE_ROUTES".into(), routes));
        }
        if let Some(auth) = self.ts_authkey() {
            env.push(("TS_AUTHKEY".into(), auth));
        }
        let models = self
            .config
            .tier_models_for_vram(node.static_capacity.vram_gb());
        if !models.is_empty() {
            env.push(("OLLAMA_MODELS".into(), models.join(" ")));
        }
        env
    }

    fn redact_output(&self, output: &str) -> String {
        let mut safe = output.to_string();
        if let Some(auth) = self.ts_authkey() {
            safe = safe.replace(&auth, "tskey-***");
        }
        safe
    }

    async fn run_remote_script(
        &self,
        node: &NodeConfig,
        script: &[u8],
        phase: &str,
    ) -> Result<(u32, String, bool), String> {
        let target = SshTarget::from_node(node).ok_or_else(|| "no ssh config".to_string())?;
        let upload = format!("cat > {REMOTE_SCRIPT} && chmod 755 {REMOTE_SCRIPT}");
        let uploaded = self.run(&target, &upload, Some(script)).await?;
        if uploaded.exit_status != 0 && !uploaded.disconnected {
            return Err(format!("script upload failed: {}", uploaded.output));
        }
        let sudo = self.run(&target, "sudo -n true", None).await?;
        if sudo.exit_status != 0 {
            return Ok((
                1,
                "passwordless sudo required (sudo -n failed)".into(),
                false,
            ));
        }
        let env = self.remote_env(node, phase);
        let mut safe_env: Vec<(String, String)> = Vec::new();
        let mut assign = String::new();
        for (k, v) in &env {
            if k == "TS_AUTHKEY" {
                safe_env.push((k.clone(), redact_authkey(Some(v))));
            } else {
                safe_env.push((k.clone(), v.clone()));
            }
            if !assign.is_empty() {
                assign.push(' ');
            }
            assign.push_str(k);
            assign.push('=');
            assign.push_str(&posix_quote(v));
        }
        tracing::info!(
            node_id = %node.id,
            phase = phase,
            ssh_endpoint = %target.endpoint(),
            env = ?safe_env,
            "provision_remote_exec"
        );
        let cmd = format!("sudo -n env {assign} bash {REMOTE_SCRIPT}");
        let out = self.run(&target, &cmd, None).await?;
        let reboot = out.disconnected || out.output.contains("REBOOT_REQUIRED=1");
        Ok((out.exit_status, out.output, reboot))
    }

    fn parse_marker(output: &str, key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        output.lines().find_map(|line| {
            let line = line.trim();
            line.strip_prefix(&prefix)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    }

    fn parse_tailscale_ip(output: &str) -> Option<String> {
        Self::parse_marker(output, "TAILSCALE_IP").filter(|ip| is_tailscale_ipv4(ip))
    }

    async fn verify_ollama(&self, tailscale_ip: &str) -> bool {
        if !is_tailscale_ipv4(tailscale_ip) {
            return false;
        }
        let url = format!("http://{tailscale_ip}:11434/api/tags");
        let Ok(resp) = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        else {
            return false;
        };
        resp.status().is_success()
    }

    async fn run_full_with_reboot(
        &self,
        node: &NodeConfig,
        script: &[u8],
    ) -> Result<(u32, String), ProvisionResult> {
        let (exit_status, output, reboot) = self
            .run_remote_script(node, script, "full")
            .await
            .map_err(|err| {
                self.fail(
                    node,
                    format!(
                        "ssh error (full): {}",
                        err.chars().take(200).collect::<String>()
                    ),
                    ProvisionPhase::ProvisionOverTailscale,
                    None,
                )
            })?;
        tracing::info!(
            node_id = %node.id,
            exit_status,
            reboot,
            ssh_endpoint = %node.ssh.as_ref().map(|s| format!("{}@{}:{}", s.user, s.host, s.port)).unwrap_or_default(),
            tail = %self.redact_output(&output).chars().rev().take(500).collect::<String>().chars().rev().collect::<String>(),
            "provision_full_phase"
        );
        if reboot || output.contains("REBOOT_REQUIRED=1") {
            let timeout = Duration::from_secs_f64(
                self.config
                    .provision_defaults
                    .reboot_wait_timeout_seconds
                    .max(1.0),
            );
            if !self
                .wait_for_ssh(
                    node,
                    timeout,
                    Duration::from_secs(8),
                    "provision_waiting_reboot",
                )
                .await
            {
                return Err(self.fail(
                    node,
                    format!(
                        "reboot wait timed out on {}",
                        node.ssh
                            .as_ref()
                            .map(|s| format!("{}@{}:{}", s.user, s.host, s.port))
                            .unwrap_or_default()
                    ),
                    ProvisionPhase::ProvisionOverTailscale,
                    None,
                ));
            }
            let (exit_status, output, reboot2) = self
                .run_remote_script(node, script, "full")
                .await
                .map_err(|err| {
                    self.fail(
                        node,
                        format!(
                            "post-reboot ssh: {}",
                            err.chars().take(200).collect::<String>()
                        ),
                        ProvisionPhase::ProvisionOverTailscale,
                        None,
                    )
                })?;
            if reboot2 {
                return Err(self.fail(
                    node,
                    "unexpected second reboot".into(),
                    ProvisionPhase::ProvisionOverTailscale,
                    None,
                ));
            }
            if exit_status != 0 {
                return Err(self.fail(
                    node,
                    format!(
                        "setup exit={exit_status}: {}",
                        output
                            .chars()
                            .rev()
                            .take(300)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ),
                    ProvisionPhase::ProvisionOverTailscale,
                    None,
                ));
            }
            return Ok((exit_status, output));
        }
        if exit_status != 0 {
            return Err(self.fail(
                node,
                format!(
                    "exit={exit_status}: {}",
                    output
                        .chars()
                        .rev()
                        .take(300)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                ),
                ProvisionPhase::ProvisionOverTailscale,
                None,
            ));
        }
        Ok((exit_status, output))
    }

    async fn provision_locked(&self, mut node: NodeConfig, opts: ProvisionOpts) -> ProvisionResult {
        let known_ts = self.known_tailscale_ip(&node);
        if let Some(ts) = known_ts.as_deref() {
            if node.ssh.as_ref().is_some_and(|s| s.host != ts) {
                let mut trial = node.clone();
                if let Some(ssh) = trial.ssh.as_mut() {
                    ssh.host = ts.to_string();
                    ssh.port = self.tailscale_ssh_port();
                }
                if let Some(target) = SshTarget::from_node(&trial) {
                    if self.probe(&target).await.is_ok() {
                        self.switch_ssh_to_tailscale(&mut node, ts);
                        tracing::info!(
                            node_id = %node.id,
                            tailscale_ip = ts,
                            "provision_resume_via_tailscale_ssh"
                        );
                    }
                }
            }
        }

        self.set_phase(&node, ProvisionPhase::WaitingPublicSsh)
            .await;
        let reachable = if let Some(target) = SshTarget::from_node(&node) {
            self.probe(&target).await.is_ok()
        } else {
            false
        };
        let mut reason = if reachable {
            "ok".to_string()
        } else {
            "ssh unreachable".to_string()
        };
        if reason != "ok" && opts.wait_for_public_ssh && known_ts.is_none() {
            let timeout = Duration::from_secs_f64(
                self.config
                    .provision_defaults
                    .reboot_wait_timeout_seconds
                    .max(1.0),
            );
            if self
                .wait_for_ssh(
                    &node,
                    timeout,
                    Duration::ZERO,
                    "provision_waiting_public_ssh",
                )
                .await
            {
                reason = "ok".into();
            } else {
                reason = "public SSH readiness timed out".into();
            }
        } else if reason != "ok" {
            if let Some(ts) = known_ts.as_deref() {
                if node.ssh.as_ref().is_some_and(|s| s.host != ts) {
                    self.switch_ssh_to_tailscale(&mut node, ts);
                    if let Some(target) = SshTarget::from_node(&node) {
                        if self.probe(&target).await.is_ok() {
                            reason = "ok".into();
                        }
                    }
                }
            }
        }

        if reason != "ok" {
            self.set_phase(&node, ProvisionPhase::Cooldown).await;
            self.mark_cooldown(node.id.as_str());
            return ProvisionResult::skip(
                node.id.clone(),
                format!("ssh unreachable: {reason}"),
                ProvisionPhase::Cooldown,
            );
        }

        if opts.dry_run {
            self.set_phase(&node, ProvisionPhase::Dry).await;
            return ProvisionResult {
                node_id: node.id.clone(),
                status: ProvisionStatus::Dry,
                detail: format!(
                    "would two-phase provision on {}",
                    node.ssh
                        .as_ref()
                        .map(|s| format!("{}@{}:{}", s.user, s.host, s.port))
                        .unwrap_or_default()
                ),
                tailscale_ip: None,
                phase: Some(ProvisionPhase::Dry.as_str().into()),
            };
        }

        let script = match read_provision_script(&self.config) {
            Ok(bytes) => bytes,
            Err(err) => return self.fail(&node, err, ProvisionPhase::Fail, None),
        };

        let mut ts_ip = known_ts
            .clone()
            .filter(|ip| node.ssh.as_ref().is_some_and(|s| s.host == *ip));
        let already_on_ts = ts_ip.as_deref().is_some_and(|ip| {
            is_tailscale_ipv4(ip) && node.ssh.as_ref().is_some_and(|s| s.host == ip)
        });

        if !already_on_ts {
            self.set_phase(&node, ProvisionPhase::BootstrapTailscale)
                .await;
            let (exit_status, output, reboot) =
                match self.run_remote_script(&node, &script, "bootstrap").await {
                    Ok(v) => v,
                    Err(err) => {
                        return self.fail(
                            &node,
                            format!(
                                "ssh error (bootstrap): {}",
                                err.chars().take(200).collect::<String>()
                            ),
                            ProvisionPhase::BootstrapTailscale,
                            None,
                        );
                    }
                };
            tracing::info!(
                node_id = %node.id,
                exit_status,
                reboot,
                tail = %self.redact_output(&output).chars().rev().take(500).collect::<String>().chars().rev().collect::<String>(),
                "provision_bootstrap"
            );
            if reboot {
                return self.fail(
                    &node,
                    "bootstrap requested reboot (unexpected); refusing public post-reboot path"
                        .into(),
                    ProvisionPhase::BootstrapTailscale,
                    None,
                );
            }
            if exit_status != 0 {
                return self.fail(
                    &node,
                    format!(
                        "bootstrap exit={exit_status}: {}",
                        output
                            .chars()
                            .rev()
                            .take(300)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ),
                    ProvisionPhase::BootstrapTailscale,
                    None,
                );
            }
            ts_ip = Self::parse_tailscale_ip(&output);
            let Some(ip) = ts_ip.clone() else {
                return self.fail(
                    &node,
                    "missing/invalid TAILSCALE_IP= after bootstrap (refusing public IP)".into(),
                    ProvisionPhase::BootstrapTailscale,
                    None,
                );
            };
            self.persist_tailscale_meta(&node, &ip, "");
            self.switch_ssh_to_tailscale(&mut node, &ip);
            self.set_phase(&node, ProvisionPhase::WaitingTailnetOpenssh)
                .await;
            let ts_wait = Duration::from_secs_f64(
                self.config
                    .provision_defaults
                    .tailscale_ssh_wait_timeout_seconds
                    .max(1.0),
            );
            if !self
                .wait_for_ssh(
                    &node,
                    ts_wait,
                    Duration::from_secs(2),
                    "provision_waiting_tailnet_openssh",
                )
                .await
            {
                return self.fail(
                    &node,
                    format!(
                        "ordinary SSH over Tailscale wait timed out on {}",
                        node.ssh
                            .as_ref()
                            .map(|s| format!("{}@{}:{}", s.user, s.host, s.port))
                            .unwrap_or_default()
                    ),
                    ProvisionPhase::WaitingTailnetOpenssh,
                    Some(ip),
                );
            }
        } else {
            tracing::info!(
                node_id = %node.id,
                tailscale_ip = ts_ip.as_deref(),
                "provision_skip_bootstrap_already_on_tailscale"
            );
        }

        self.set_phase(&node, ProvisionPhase::ProvisionOverTailscale)
            .await;
        let (_exit, output) = match self.run_full_with_reboot(&node, &script).await {
            Ok(v) => v,
            Err(mut fail) => {
                if fail.tailscale_ip.is_none() {
                    fail.tailscale_ip = ts_ip.clone();
                }
                return fail;
            }
        };
        if let Some(parsed) = Self::parse_tailscale_ip(&output) {
            ts_ip = Some(parsed);
        }
        let Some(ip) = ts_ip.clone().filter(|ip| is_tailscale_ipv4(ip)) else {
            return self.fail(
                &node,
                "missing/invalid TAILSCALE_IP= in full provision output (refusing public IP)"
                    .into(),
                ProvisionPhase::ProvisionOverTailscale,
                None,
            );
        };
        if node.ssh.as_ref().is_some_and(|s| s.host != ip) {
            self.switch_ssh_to_tailscale(&mut node, &ip);
        }
        if let Some(target) = SshTarget::from_node(&node) {
            if let Err(reason) = self.probe(&target).await {
                return self.fail(
                    &node,
                    format!("ordinary SSH over Tailscale unavailable: {reason}"),
                    ProvisionPhase::ProvisionOverTailscale,
                    Some(ip),
                );
            }
        }

        self.set_phase(&node, ProvisionPhase::VerifyOllama).await;
        if !self.verify_ollama(&ip).await {
            return self.fail(
                &node,
                format!("ollama /api/tags failed over Tailscale (ts_ip={ip})"),
                ProvisionPhase::VerifyOllama,
                Some(ip.clone()),
            );
        }
        let url = match self.apply_tailscale_url(&mut node, &ip) {
            Ok(url) => url,
            Err(err) => {
                return self.fail(&node, err, ProvisionPhase::VerifyOllama, Some(ip));
            }
        };
        self.clear_cooldown(node.id.as_str());
        self.set_phase(&node, ProvisionPhase::Ok).await;
        ProvisionResult {
            node_id: node.id,
            status: ProvisionStatus::Ok,
            detail: format!("provisioned; url={url}"),
            tailscale_ip: Some(ip),
            phase: Some(ProvisionPhase::Ok.as_str().into()),
        }
    }
}

impl NodeProvisioner for ProvisionOrchestrator {
    fn provision_node(&self, node: NodeConfig, opts: ProvisionOpts) -> ProvisionFuture<'_> {
        Box::pin(self.provision_node_inner(node, opts))
    }
}

/// Register with url=None, persist Verda meta without public :11434, wait for public SSH.
pub async fn provision_new_tailscale(
    provisioner: &ProvisionOrchestrator,
    registry: &Registry,
    node: NodeConfig,
) -> ProvisionResult {
    let mut node = node;
    node.url = None;
    registry.upsert_verda(node.clone());
    provisioner
        .provision_node_inner(
            node,
            ProvisionOpts {
                wait_for_public_ssh: true,
                ..ProvisionOpts::default()
            },
        )
        .await
}

/// Hydrate Tailscale URL if present; else provision without waiting for public SSH.
pub async fn adopt_with_tailscale(
    provisioner: &ProvisionOrchestrator,
    registry: &Registry,
    fleet_state: Option<&FleetState>,
    mut node: NodeConfig,
) -> ProvisionResult {
    if let Some(fs) = fleet_state {
        if let Ok(Some(url)) = fs.hydrate_url(&node.id) {
            node.url = Some(url.clone());
            if let Some(ip) = url_host_tailscale_ip(&url) {
                if let Some(ssh) = node.ssh.as_mut() {
                    ssh.host = ip;
                    ssh.port = provisioner.tailscale_ssh_port();
                }
            }
            registry.upsert_verda(node.clone());
            let _ = registry.set_node_url(&node.id, &url);
            return ProvisionResult {
                node_id: node.id,
                status: ProvisionStatus::Ok,
                detail: "hydrated".into(),
                tailscale_ip: url_host_tailscale_ip(&url),
                phase: Some("ok".into()),
            };
        }
        if let Ok(Some(entry)) = fs.get_entry(node.id.as_str()) {
            if let Some(ip) = entry
                .tailscale_ip
                .as_deref()
                .filter(|ip| is_tailscale_ipv4(ip))
            {
                if let Some(ssh) = node.ssh.as_mut() {
                    ssh.host = ip.to_string();
                    ssh.port = provisioner.tailscale_ssh_port();
                }
            }
        }
    }
    node.url = None;
    registry.upsert_verda(node.clone());
    provisioner
        .provision_node_inner(
            node,
            ProvisionOpts {
                wait_for_public_ssh: false,
                ..ProvisionOpts::default()
            },
        )
        .await
}

fn url_host_tailscale_ip(url: &str) -> Option<String> {
    if !url_host_is_tailscale(url) {
        return None;
    }
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}
