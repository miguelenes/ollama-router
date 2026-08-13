//! Background watcher: provision hosts when Ollama is down but SSH is up.

use std::sync::Arc;
use std::time::Duration;

use ollama_router_core::provision::{ProvisionOpts, ProvisionStatus};

use super::orchestrator::ProvisionOrchestrator;
use ollama_router_core::fleet::Registry;

/// Poll unhealthy provisionable nodes and trigger SSH provision.
pub struct ProvisionWatcher {
    registry: Arc<Registry>,
    orchestrator: Arc<ProvisionOrchestrator>,
}

impl ProvisionWatcher {
    pub fn new(registry: Arc<Registry>, orchestrator: Arc<ProvisionOrchestrator>) -> Self {
        Self {
            registry,
            orchestrator,
        }
    }

    pub fn enabled(&self) -> bool {
        self.orchestrator.auto_enabled()
    }

    pub async fn run(self, auto: bool, poll_interval: f64) {
        if !auto {
            tracing::info!("provision_watcher_disabled");
            return;
        }
        tracing::info!(poll_interval, "provision_watcher_started");
        let interval = Duration::from_secs_f64(poll_interval.max(0.5));
        tokio::time::sleep(interval.min(Duration::from_secs(5))).await;
        loop {
            if let Err(err) = self.tick().await {
                tracing::error!(error = %err, "provision_watcher_tick_failed");
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn tick(&self) -> Result<(), String> {
        let snaps = self.registry.snapshot();
        for snap in snaps {
            if snap.healthy {
                continue;
            }
            let Some(mut node) = self.registry.node_config(&snap.id) else {
                continue;
            };
            if !node.provision_enabled() {
                continue;
            }
            if self.orchestrator.is_inflight(node.id.as_str()) {
                continue;
            }
            if self.orchestrator.on_cooldown(node.id.as_str()) {
                continue;
            }
            if let Some(ssh) = node.ssh.as_mut() {
                if let Some(url) = node.url.as_deref() {
                    if let Ok(parsed) = url::Url::parse(url) {
                        if let Some(host) = parsed.host_str() {
                            if ollama_router_core::fleet::is_tailscale_ipv4(host)
                                && ssh.host != host
                            {
                                ssh.host = host.to_string();
                            }
                        }
                    }
                }
            }
            let Some(target) = super::orchestrator::SshTarget::from_node(&node) else {
                continue;
            };
            if self.orchestrator.probe(&target).await.is_err() {
                tracing::info!(node_id = %node.id, "provision_watcher_ssh_skip");
                self.orchestrator.mark_cooldown(node.id.as_str());
                continue;
            }
            tracing::info!(
                node_id = %node.id,
                ollama_url = node.url.as_deref(),
                ssh_host = node.ssh.as_ref().map(|s| s.host.as_str()),
                "provision_watcher_trigger"
            );
            let orch = self.orchestrator.clone();
            tokio::spawn(async move {
                let result = orch
                    .provision_node_inner(node, ProvisionOpts::default())
                    .await;
                tracing::info!(
                    node_id = %result.node_id,
                    status = result.status.as_str(),
                    detail = %result.detail.chars().take(200).collect::<String>(),
                    tailscale_ip = result.tailscale_ip.as_deref(),
                    phase = result.phase.as_deref(),
                    "provision_watcher_finished"
                );
                if result.status == ProvisionStatus::Fail {
                    tracing::warn!(
                        node_id = %result.node_id,
                        phase = result.phase.as_deref(),
                        "provision_watcher_unfinished"
                    );
                }
            });
        }
        Ok(())
    }
}
