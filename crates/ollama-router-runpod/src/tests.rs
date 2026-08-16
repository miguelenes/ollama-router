//! httpmock coverage for the RunPod client and manager. Never hits live RunPod.
#![allow(clippy::field_reassign_with_default)]

use std::sync::Arc;
use std::time::Duration;

use httpmock::prelude::*;
use serde_json::{json, Value};

use ollama_router_core::cloud::DemandScale;
use ollama_router_core::config::{Capacity, NodeConfig, RouterConfig, RunpodConfig};
use ollama_router_core::fleet::{
    CloudInstanceId, EnrollPersist, FleetState, NodeId, Registry, RunpodNodePersist,
};
use ollama_router_core::routing::RoutingError;
use tokio_util::sync::CancellationToken;

use crate::client::RunpodClient;
use crate::manager::{RunpodManager, MANAGED_BY_MARKER};

fn client(server: &MockServer) -> RunpodClient {
    let config = RunpodConfig {
        base_url_v1: server.base_url(),
        base_url_v2: server.base_url(),
        ..RunpodConfig::default()
    };
    RunpodClient::with_api_key(config, "test-key".into()).expect("client")
}

fn owned_name(suffix: &str) -> String {
    // Match default router_id slug from hostname fallback — tests set OLLAMA_ROUTER_ID.
    format!("{MANAGED_BY_MARKER}-test-{suffix}")
}

fn owned_pod(id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "name": owned_name(id),
        "desiredStatus": status,
        "costPerHr": 0.30,
        "machine": {"dataCenterId": "US-CA-2", "gpuTypeId": "NVIDIA L4"},
    })
}

fn foreign_pod(id: &str) -> Value {
    json!({
        "id": id,
        "name": "someone-elses-pod",
        "desiredStatus": "RUNNING",
        "costPerHr": 0.30,
    })
}

fn stub_catalog(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/catalog/gpus");
        then.status(200).json_body(json!({
            "gpus": [{
                "id": "NVIDIA L4",
                "name": "NVIDIA L4",
                "manufacturer": "NVIDIA",
                "memory": 24,
                "price": {"secure": 0.39, "community": 0.30},
                "availability": "HIGH",
                "extraFutureField": true,
            }]
        }));
    });
}

fn stub_tags(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(json!({"models": []}));
    });
}

fn persist_test_enroll(fs: &FleetState, node_id: &str, base: &str) {
    fs.persist_enroll(
        node_id,
        EnrollPersist {
            url: base,
            capacity_url: &format!("{base}/v1/capacity"),
            ollama_share_id: "share-ollama",
            agent_share_id: "share-agent",
        },
    )
    .expect("enroll");
}

fn persist_owned(fs: &FleetState, id: &str) {
    let pid = CloudInstanceId::parse(id).unwrap();
    fs.persist_runpod_node(
        format!("runpod-{id}"),
        RunpodNodePersist {
            url: "http://127.0.0.1:41990",
            pod_id: &pid,
            gpu_type: "NVIDIA L4",
            data_center: Some("US-CA-2"),
            cost_per_hour: Some(0.30),
            hostname: Some(&owned_name(id)),
        },
    )
    .unwrap();
}

fn runpod_node(id: &str) -> NodeConfig {
    NodeConfig {
        id: NodeId::parse(format!("runpod-{id}")).unwrap(),
        url: Some("http://127.0.0.1:41990".into()),
        capacity_url: None,
        labels: vec!["gpu".into(), "runpod".into(), "spot".into()],
        static_capacity: Capacity::default(),
        max_inflight: None,
    }
}

fn manager_with(
    server: &MockServer,
    tweak: impl FnOnce(&mut RouterConfig),
) -> (RunpodManager, Arc<Registry>, Arc<FleetState>) {
    manager_with_shutdown(server, CancellationToken::new(), tweak)
}

fn manager_with_shutdown(
    server: &MockServer,
    shutdown: CancellationToken,
    tweak: impl FnOnce(&mut RouterConfig),
) -> (RunpodManager, Arc<Registry>, Arc<FleetState>) {
    std::env::set_var("OLLAMA_ROUTER_ID", "test");
    let dir = tempfile::tempdir().expect("tmp");
    let fs = Arc::new(FleetState::new(dir.path().join("state.json")));
    std::mem::forget(dir);
    let mut config = RouterConfig::default();
    config.runpod.enabled = true;
    config.runpod.base_url_v1 = server.base_url();
    config.runpod.base_url_v2 = server.base_url();
    config.runpod.poll_interval_seconds = 0.5;
    config.runpod.create_timeout_seconds = 5.0;
    config.runpod.router_id_env = "OLLAMA_ROUTER_ID".into();
    tweak(&mut config);
    let config = Arc::new(config);
    let registry = Arc::new(Registry::new(&config));
    let client = client(server);
    let mgr = RunpodManager::with_shutdown(config, client, registry.clone(), fs.clone(), shutdown)
        .expect("runpod manager");
    (mgr, registry, fs)
}

fn manager(
    server: &MockServer,
    auto_scale: bool,
) -> (RunpodManager, Arc<Registry>, Arc<FleetState>) {
    manager_with(server, |c| c.runpod.auto_scale = auto_scale)
}

#[tokio::test]
async fn client_lists_catalog_ignoring_unknown_fields() {
    let server = MockServer::start();
    stub_catalog(&server);
    let gpus = client(&server).list_catalog_gpus().await.expect("catalog");
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].gpu_type_id(), Some("NVIDIA L4"));
    assert_eq!(gpus[0].memory, Some(24.0));
}

#[tokio::test]
async fn client_retries_on_429_honoring_retry_after() {
    let server = MockServer::start();
    let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first = n.clone();
    server.mock(|when, then| {
        when.method(GET)
            .path("/pods")
            .is_true(move |_| first.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0);
        then.status(429)
            .header("Retry-After", "0")
            .json_body(json!({"title": "rate limited"}));
    });
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([]));
    });
    let pods = client(&server).list_pods().await.expect("pods");
    assert!(pods.is_empty());
}

#[tokio::test]
async fn demand_scale_up_coalesces_create_additional() {
    let server = MockServer::start();
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(200).json_body(json!({
            "id": "pod-d",
            "name": owned_name("pod-d"),
            "desiredStatus": "RUNNING",
            "costPerHr": 0.30,
            "machine": {"dataCenterId": "US-CA-2", "gpuTypeId": "NVIDIA L4"},
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/pods/pod-d");
        then.status(200).json_body(json!({
            "id": "pod-d",
            "name": owned_name("pod-d"),
            "desiredStatus": "RUNNING",
            "costPerHr": 0.30,
        }));
    });
    let (mgr, _, fs) = manager(&server, true);
    persist_test_enroll(&fs, "runpod-pod-d", &server.base_url());
    mgr.request_scale_up(RoutingError::NoHealthy);
    mgr.request_scale_up(RoutingError::Saturated);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(post.calls(), 1, "coalesced demand must create once");
}

#[tokio::test]
async fn demand_scale_up_respects_max_instances() {
    let server = MockServer::start();
    stub_catalog(&server);
    let post = server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(200).json_body(json!({"id": "should-not"}));
    });
    let (mgr, registry, _) = manager_with(&server, |c| {
        c.runpod.auto_scale = true;
        c.runpod.auto_scale_max_instances = 1;
    });
    registry.upsert_runpod(runpod_node("already"));
    mgr.request_scale_up(RoutingError::Saturated);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(post.calls(), 0);
}

#[tokio::test]
async fn foreign_pod_is_untouched() {
    let server = MockServer::start();
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([
            foreign_pod("foreign-1"),
            owned_pod("owned-1", "RUNNING"),
        ]));
    });
    let delete = server.mock(|when, then| {
        when.method(DELETE).path("/pods/foreign-1");
        then.status(200).json_body(json!({"ok": true}));
    });
    let (mgr, registry, fs) = manager_with(&server, |c| {
        c.runpod.auto_scale = false;
        c.runpod.orphan_reclaim_enabled = true;
        c.runpod.orphan_reclaim_grace_seconds = 0.0;
    });
    persist_owned(&fs, "owned-1");
    registry.upsert_runpod(runpod_node("owned-1"));
    mgr.reconcile().await;
    assert_eq!(delete.calls(), 0);
}

#[tokio::test]
async fn interrupted_below_floor_is_replaced() {
    let server = MockServer::start();
    stub_catalog(&server);
    stub_tags(&server);
    let list_n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let list_c = list_n.clone();
    server.mock(|when, then| {
        when.method(GET)
            .path("/pods")
            .is_true(move |_| list_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2);
        then.status(200)
            .json_body(json!([owned_pod("pod-x", "EXITED")]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([]));
    });
    let delete = server.mock(|when, then| {
        when.method(DELETE).path("/pods/pod-x");
        then.status(200).json_body(json!({"ok": true}));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(200).json_body(json!({
            "id": "pod-new",
            "name": owned_name("pod-new"),
            "desiredStatus": "RUNNING",
            "costPerHr": 0.30,
            "machine": {"gpuTypeId": "NVIDIA L4"},
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/pods/pod-new");
        then.status(200).json_body(json!({
            "id": "pod-new",
            "name": owned_name("pod-new"),
            "desiredStatus": "RUNNING",
            "costPerHr": 0.30,
        }));
    });
    let (mgr, _, fs) = manager_with(&server, |c| {
        c.runpod.auto_scale = true;
        c.runpod.auto_scale_min_instances = 1;
        c.runpod.auto_scale_max_instances = 2;
        c.runpod.orphan_reclaim_enabled = false;
    });
    persist_owned(&fs, "pod-x");
    persist_test_enroll(&fs, "runpod-pod-new", &server.base_url());
    mgr.reconcile().await;
    assert!(delete.calls() >= 1);
    assert!(post.calls() >= 1, "below floor must replace");
}

#[tokio::test]
async fn interrupted_above_floor_waits_for_demand() {
    let server = MockServer::start();
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([
            owned_pod("pod-keep", "RUNNING"),
            owned_pod("pod-dead", "EXITED"),
        ]));
    });
    let delete = server.mock(|when, then| {
        when.method(DELETE).path("/pods/pod-dead");
        then.status(200).json_body(json!({"ok": true}));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(200).json_body(json!({"id": "should-not"}));
    });
    let (mgr, registry, fs) = manager_with(&server, |c| {
        c.runpod.auto_scale = true;
        c.runpod.auto_scale_min_instances = 1;
        c.runpod.auto_scale_max_instances = 4;
        c.runpod.orphan_reclaim_enabled = false;
    });
    persist_owned(&fs, "pod-keep");
    persist_owned(&fs, "pod-dead");
    registry.upsert_runpod(runpod_node("pod-keep"));
    mgr.reconcile().await;
    assert_eq!(delete.calls(), 1);
    assert_eq!(post.calls(), 0, "above floor must not auto-replace");
}

#[tokio::test]
async fn failed_destroy_retains_fleet_state() {
    let server = MockServer::start();
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200)
            .json_body(json!([owned_pod("pod-idle", "RUNNING")]));
    });
    server.mock(|when, then| {
        when.method(DELETE).path("/pods/pod-idle");
        then.status(500).json_body(json!({"title": "busy"}));
    });
    let (mgr, registry, fs) = manager_with(&server, |c| {
        c.runpod.auto_scale = true;
        c.runpod.auto_scale_min_instances = 0;
        c.runpod.idle_scale_down_enabled = true;
        c.runpod.idle_timeout_seconds = 0.0;
        c.runpod.idle_grace_after_create_seconds = 0.0;
        c.runpod.orphan_reclaim_enabled = false;
    });
    let nid = NodeId::parse("runpod-pod-idle").unwrap();
    registry.upsert_runpod(runpod_node("pod-idle"));
    persist_owned(&fs, "pod-idle");
    mgr.reconcile().await;
    assert!(
        fs.list_runpod_nodes()
            .unwrap()
            .contains_key("runpod-pod-idle"),
        "failed destroy must retain FleetState"
    );
    assert!(registry.get(&nid).is_some());
}

#[tokio::test]
async fn create_failure_logs_no_secrets() {
    let server = MockServer::start();
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200).json_body(json!([]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(400)
            .json_body(json!({"title": "bad", "detail": "secret-should-not-surface"}));
    });
    std::env::set_var("ZROK_ENABLE_TOKEN", "super-secret-zrok");
    std::env::set_var("OLLAMA_ROUTER_ADMIN_TOKEN", "super-secret-admin");
    let (mgr, _, _) = manager_with(&server, |c| {
        c.runpod.auto_scale = false;
        c.runpod.interruptible = true;
        c.runpod.on_demand_fallback = false;
    });
    let err = mgr.create_additional().await;
    // Create returns Ok(none) on interruptible stockout without fallback, or Err.
    // Either way the error/Display must not include secrets or response bodies.
    let msg = match &err {
        Ok(v) => v.to_string(),
        Err(e) => e.to_string(),
    };
    assert!(!msg.contains("super-secret"), "{msg}");
    assert!(!msg.contains("secret-should-not-surface"), "{msg}");
    assert!(!msg.contains("test-key"), "{msg}");
}

#[tokio::test]
async fn create_additional_refuses_when_live_owned_at_max() {
    let server = MockServer::start();
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/pods");
        then.status(200)
            .json_body(json!([owned_pod("pod-1", "RUNNING")]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/pods");
        then.status(200).json_body(json!({"id": "nope"}));
    });
    let (mgr, _, _) = manager_with(&server, |c| {
        c.runpod.auto_scale = true;
        c.runpod.auto_scale_max_instances = 1;
    });
    let out = mgr.create_additional().await.expect("create");
    assert_eq!(out["status"], "none");
    assert_eq!(post.calls(), 0);
}
