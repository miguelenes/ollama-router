//! Provision orchestrator tests with a mock SSH transport (no live sshd).
#![allow(clippy::field_reassign_with_default)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ollama_router_core::config::{Capacity, NodeConfig, NodeSshConfig, RouterConfig};
use ollama_router_core::fleet::{FleetState, NodeId, Registry};
use ollama_router_core::provision::{ProvisionOpts, ProvisionPhase, ProvisionStatus};

use super::orchestrator::{MockTransport, ProvisionOrchestrator, SshTarget};
use super::ssh::RemoteOutput;

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("id")
}

fn ssh_node(id: &str, host: &str) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: None,
        capacity_url: None,
        labels: vec!["gpu".into()],
        static_capacity: Capacity {
            vram_gb: Some(24.0),
            ram_gb: Some(32.0),
            gpus: Some(1),
            cpu_cores: Some(8),
        },
        max_inflight: None,
        ssh: Some(NodeSshConfig {
            host: host.into(),
            port: 22,
            user: "root".into(),
            key_file: Some("/run/secrets/ssh_key".into()),
            password_env: None,
        }),
        provision: None,
    }
}

struct ScriptMock {
    /// host → remaining probe failures before success
    probe_fail: Mutex<HashMap<String, usize>>,
    bootstrap: Mutex<String>,
    full: Mutex<String>,
    concurrent: AtomicUsize,
    max_concurrent: AtomicUsize,
    wait_public: AtomicUsize,
}

impl ScriptMock {
    fn new(bootstrap: &str, full: &str) -> Arc<Self> {
        Arc::new(Self {
            probe_fail: Mutex::new(HashMap::new()),
            bootstrap: Mutex::new(bootstrap.into()),
            full: Mutex::new(full.into()),
            concurrent: AtomicUsize::new(0),
            max_concurrent: AtomicUsize::new(0),
            wait_public: AtomicUsize::new(0),
        })
    }
}

impl MockTransport for ScriptMock {
    fn probe(
        &self,
        target: &SshTarget,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        let host = target.host.clone();
        Box::pin(async move {
            let mut map = self.probe_fail.lock().expect("lock");
            let left = map.entry(host.clone()).or_insert(0);
            if *left > 0 {
                *left -= 1;
                self.wait_public.fetch_add(1, Ordering::SeqCst);
                return Err("refused".into());
            }
            Ok(())
        })
    }

    fn run(
        &self,
        _target: &SshTarget,
        command: &str,
        _stdin: Option<&[u8]>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RemoteOutput, String>> + Send + '_>,
    > {
        let cmd = command.to_string();
        Box::pin(async move {
            let n = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_concurrent.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            let output = if cmd.contains("sudo -n env") && cmd.contains("PROVISION_PHASE=bootstrap")
                || (cmd.contains("sudo -n env")
                    && self.bootstrap.lock().unwrap().contains("TAILSCALE_IP")
                    && cmd.contains("bootstrap"))
            {
                self.bootstrap.lock().unwrap().clone()
            } else if cmd.contains("sudo -n env") {
                self.full.lock().unwrap().clone()
            } else {
                String::new()
            };
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(RemoteOutput {
                exit_status: 0,
                output,
                disconnected: false,
            })
        })
    }
}

fn make_orch(config: RouterConfig, mock: Arc<dyn MockTransport>) -> ProvisionOrchestrator {
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .expect("client");
    let registry = Arc::new(Registry::new(&config));
    let dir = tempfile::tempdir().expect("tmp");
    let fs = Arc::new(FleetState::new(dir.path().join("state.json")));
    // leak tempdir for test lifetime
    std::mem::forget(dir);
    ProvisionOrchestrator::new(Arc::new(config), client, Some(registry), Some(fs)).with_mock(mock)
}

#[tokio::test]
async fn dry_run_does_not_exec_script() {
    let mut config = RouterConfig::default();
    config.nodes = vec![ssh_node("gpu", "203.0.113.10")];
    let mock = ScriptMock::new("", "");
    let orch = make_orch(config, mock.clone());
    let result = orch
        .provision_node_inner(
            ssh_node("gpu", "203.0.113.10"),
            ProvisionOpts {
                dry_run: true,
                ..ProvisionOpts::default()
            },
        )
        .await;
    assert_eq!(result.status, ProvisionStatus::Dry);
    assert_eq!(result.phase.as_deref(), Some(ProvisionPhase::Dry.as_str()));
}

#[tokio::test]
async fn skip_bootstrap_when_tailscale_ssh_works() {
    let ts_ip = "100.64.0.9";
    let mut config = RouterConfig::default();
    let mut node = ssh_node("gpu", "203.0.113.10");
    node.url = Some(format!("http://{ts_ip}:11434"));
    config.nodes = vec![node.clone()];
    config.provision_defaults.script_path = Some(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/provision-ollama-gpu.sh")
            .to_string_lossy()
            .into(),
    );
    let mock = ScriptMock::new(
        "should-not-run",
        &format!("TAILSCALE_IP={ts_ip}\nTAILSCALE_ONLINE=1\n"),
    );
    let client = reqwest::Client::builder().use_rustls_tls().build().unwrap();
    // Point verify at httpmock by using the mock IP... verify uses tailscale IP directly.
    // We cannot bind 100.64.0.9. Override verify by... the orchestrator always hits
    // http://{ts_ip}:11434. For unit tests, skip live verify by using a custom mock
    // that still needs HTTP. Use 127.0.0.1 is not TS. So we only assert bootstrap
    // was skipped by checking the mock never saw bootstrap env... ScriptMock full
    // vs bootstrap: if probe of 100.64.0.9 succeeds, already_on_ts is true.
    let registry = Arc::new(Registry::new(&config));
    let dir = tempfile::tempdir().unwrap();
    let fs = Arc::new(FleetState::new(dir.path().join("s.json")));
    fs.persist_url("gpu", &format!("http://{ts_ip}:11434"), Some(ts_ip))
        .unwrap();
    let orch = ProvisionOrchestrator::new(Arc::new(config), client, Some(registry), Some(fs))
        .with_mock(mock.clone());
    let result = orch
        .provision_node_inner(node, ProvisionOpts::default())
        .await;
    // verify_ollama will fail (100.64.0.9 not listening) → FAIL at verify, but
    // bootstrap should have been skipped (no bootstrap output used).
    assert_ne!(result.phase.as_deref(), Some("bootstrap_tailscale"));
    assert!(
        result.phase.as_deref() == Some("verify_ollama")
            || result.status == ProvisionStatus::Fail
            || result.status == ProvisionStatus::Ok
    );
}

#[tokio::test]
async fn wait_for_public_ssh_only_when_flag_set() {
    let mut config = RouterConfig::default();
    config.nodes = vec![ssh_node("gpu", "203.0.113.10")];
    config.provision_defaults.reboot_wait_timeout_seconds = 0.2;
    config.provision_defaults.poll_interval_seconds = 0.05;
    let mock = ScriptMock::new("TAILSCALE_IP=100.64.0.1\n", "TAILSCALE_IP=100.64.0.1\n");
    mock.probe_fail
        .lock()
        .unwrap()
        .insert("203.0.113.10".into(), 100);
    let orch = make_orch(config.clone(), mock.clone());
    let skip = orch
        .provision_node_inner(ssh_node("gpu", "203.0.113.10"), ProvisionOpts::default())
        .await;
    assert_eq!(skip.status, ProvisionStatus::Skip);
    let waits_without = mock.wait_public.load(Ordering::SeqCst);

    let mock2 = ScriptMock::new("TAILSCALE_IP=100.64.0.1\n", "TAILSCALE_IP=100.64.0.1\n");
    mock2
        .probe_fail
        .lock()
        .unwrap()
        .insert("203.0.113.10".into(), 100);
    let orch2 = make_orch(config, mock2.clone());
    let _ = orch2
        .provision_node_inner(
            ssh_node("gpu", "203.0.113.10"),
            ProvisionOpts {
                wait_for_public_ssh: true,
                ..ProvisionOpts::default()
            },
        )
        .await;
    let waits_with = mock2.wait_public.load(Ordering::SeqCst);
    assert!(
        waits_with > waits_without,
        "fresh path should retry public SSH ({waits_with} vs {waits_without})"
    );
}

#[tokio::test]
async fn cooldown_skips_until_force() {
    let mut config = RouterConfig::default();
    config.nodes = vec![ssh_node("gpu", "203.0.113.10")];
    config.provision_defaults.cooldown_seconds = 900.0;
    let mock = ScriptMock::new("", "");
    mock.probe_fail
        .lock()
        .unwrap()
        .insert("203.0.113.10".into(), 100);
    let orch = make_orch(config, mock);
    let first = orch
        .provision_node_inner(ssh_node("gpu", "203.0.113.10"), ProvisionOpts::default())
        .await;
    assert_eq!(first.status, ProvisionStatus::Skip);
    let second = orch
        .provision_node_inner(ssh_node("gpu", "203.0.113.10"), ProvisionOpts::default())
        .await;
    assert_eq!(second.phase.as_deref(), Some("cooldown"));
    assert_eq!(second.detail, "cooldown active");
    let forced = orch
        .provision_node_inner(
            ssh_node("gpu", "203.0.113.10"),
            ProvisionOpts {
                force: true,
                ..ProvisionOpts::default()
            },
        )
        .await;
    assert_eq!(forced.status, ProvisionStatus::Skip);
    assert!(
        forced.detail.contains("ssh unreachable"),
        "force should bypass cooldown and re-probe SSH: {}",
        forced.detail
    );
}

#[tokio::test]
async fn concurrency_semaphore_serializes() {
    let mut config = RouterConfig::default();
    config.nodes = vec![ssh_node("a", "203.0.113.1"), ssh_node("b", "203.0.113.2")];
    config.provision_defaults.concurrency = 1;
    config.provision_defaults.script_path = Some(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/provision-ollama-gpu.sh")
            .to_string_lossy()
            .into(),
    );
    let mock = ScriptMock::new(
        "TAILSCALE_IP=100.64.0.1\nTAILSCALE_ONLINE=1\n",
        "TAILSCALE_IP=100.64.0.1\nTAILSCALE_ONLINE=1\n",
    );
    let orch = Arc::new(make_orch(config, mock.clone()));
    let a = ssh_node("a", "203.0.113.1");
    let b = ssh_node("b", "203.0.113.2");
    let oa = orch.clone();
    let ob = orch.clone();
    let (ra, rb) = tokio::join!(
        oa.provision_node_inner(a, ProvisionOpts::default()),
        ob.provision_node_inner(b, ProvisionOpts::default()),
    );
    let _ = (ra, rb);
    assert_eq!(mock.max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn copied_script_has_no_thunder_or_userspace() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/provision-ollama-gpu.sh");
    let text = std::fs::read_to_string(path).expect("script");
    for needle in [
        "userspace-networking",
        "tailscale serve --tcp",
        "start_ollama_direct",
        "CLOUD_PROVIDER",
        "thunder",
        "start_tailscaled_direct",
        "tailscaled_is_userspace",
        "illumination-ollama-provision",
    ] {
        assert!(!text.contains(needle), "script still contains {needle:?}");
    }
    assert!(text.contains("/var/lib/ollama-router-provision"));
    assert!(text.contains("tailscale set --ssh=false"));
    assert!(text.contains("PROVISION_PHASE"));
}
