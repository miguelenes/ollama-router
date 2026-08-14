//! httpmock coverage for the Verda client and manager. Never hits live Verda.
#![allow(clippy::field_reassign_with_default)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use httpmock::Mock;
use serde_json::{json, Value};

use ollama_router_core::cloud::DemandScale;
use ollama_router_core::config::{Capacity, NodeConfig, RouterConfig, VerdaConfig};
use ollama_router_core::fleet::{
    EnrollPersist, FleetState, NodeId, Registry, VerdaInstanceId, VerdaNodePersist,
};
use ollama_router_core::routing::RoutingError;

use crate::client::VerdaClient;
use crate::manager::{VerdaManager, MANAGED_BY};
use crate::types::{Instance, StartupScript};

fn client(server: &MockServer) -> VerdaClient {
    let config = VerdaConfig {
        base_url: server.base_url(),
        ..VerdaConfig::default()
    };
    VerdaClient::with_credentials(config, "cid".into(), "csecret".into()).expect("client")
}

fn token_ok<'a>(server: &'a MockServer, expires_in: u64) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(POST).path("/v1/oauth2/token");
        then.status(200).json_body(json!({
            "access_token": "tok-1",
            "refresh_token": "ref-1",
            "expires_in": expires_in,
        }));
    })
}

fn owned_instance(id: &str) -> Value {
    json!({
        "id": id,
        "status": "running",
        "ip_address": "203.0.113.10",
        "location_code": "HEL",
        "instance_type": "gpu-l4",
        "os_volume_id": "vol-1",
        "tags": [
            {"key": "managed_by", "value": MANAGED_BY},
        ],
    })
}

fn gpu_l4_type() -> Value {
    json!({
        "instance_type": "gpu-l4",
        "manufacturer": "NVIDIA",
        "spot_price": "0.30",
        "price_per_hour": "1.20",
        "gpu": {"number_of_gpus": 1, "manufacturer": "NVIDIA", "model": "L4"},
        "gpu_memory": {"size_in_gigabytes": 24},
        "supported_os": ["ubuntu-24.04-cuda-docker"],
    })
}

fn stub_catalog_core(server: &MockServer) -> Mock<'_> {
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-availability");
        then.status(200)
            .json_body(json!([{"location_code": "HEL", "availabilities": ["gpu-l4"]}]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-types");
        then.status(200).json_body(json!([gpu_l4_type()]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/images");
        then.status(200)
            .json_body(json!([{"image_type": "ubuntu-24.04-cuda-docker"}]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/sshkeys");
        then.status(200)
            .json_body(json!([{"id": "key-1", "name": "ollama-router"}]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-availability/gpu-l4");
        then.status(200).json_body(true);
    })
}

fn stub_named_script(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/v1/scripts");
        then.status(200).json_body(json!([{
            "id": "script-1",
            "name": "ollama-router-agent-init",
        }]));
    });
}

fn stub_catalog(server: &MockServer) -> Mock<'_> {
    let confirm = stub_catalog_core(server);
    stub_named_script(server);
    confirm
}

fn stub_tags(server: &MockServer) -> Mock<'_> {
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200).json_body(json!({"models": []}));
    })
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

fn manager_with(
    server: &MockServer,
    tweak: impl FnOnce(&mut RouterConfig),
) -> (VerdaManager, Arc<Registry>, Arc<FleetState>) {
    let dir = tempfile::tempdir().expect("tmp");
    let fs = Arc::new(FleetState::new(dir.path().join("state.json")));
    std::mem::forget(dir);
    let mut config = RouterConfig::default();
    config.verda.enabled = true;
    config.verda.base_url = server.base_url();
    config.verda.ssh_key_id = Some("key-1".into());
    config.verda.poll_interval_seconds = 0.5;
    config.verda.create_timeout_seconds = 5.0;
    tweak(&mut config);
    let config = Arc::new(config);
    let registry = Arc::new(Registry::new(&config));
    let client = client(server);
    let mgr = VerdaManager::new(config, client, registry.clone(), fs.clone());
    (mgr, registry, fs)
}

fn manager(
    server: &MockServer,
    auto_scale: bool,
) -> (VerdaManager, Arc<Registry>, Arc<FleetState>) {
    manager_with(server, |c| c.verda.auto_scale = auto_scale)
}

#[tokio::test]
async fn client_credentials_grant() {
    let server = MockServer::start();
    let token = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/oauth2/token")
            .json_body_includes(r#"{"grant_type":"client_credentials","client_id":"cid"}"#);
        then.status(200).json_body(json!({
            "access_token": "tok-1",
            "expires_in": 3600
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-types");
        then.status(200).json_body(json!([]));
    });
    let types = client(&server).get_instance_types().await.expect("types");
    assert!(types.is_empty());
    token.assert();
}

#[tokio::test]
async fn client_retries_once_on_401() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    let n = Arc::new(AtomicUsize::new(0));
    let first = n.clone();
    server.mock(|when, then| {
        when.method(GET)
            .path("/v1/instance-types")
            .is_true(move |_| first.fetch_add(1, Ordering::SeqCst) == 0);
        then.status(401).json_body(json!({"error": "expired"}));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-types");
        then.status(200).json_body(json!([]));
    });
    let types = client(&server).get_instance_types().await.expect("types");
    assert!(types.is_empty());
}

#[tokio::test]
async fn client_honors_429_retry_after() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    let n = Arc::new(AtomicUsize::new(0));
    let first = n.clone();
    server.mock(|when, then| {
        when.method(GET)
            .path("/v1/instance-types")
            .is_true(move |_| first.fetch_add(1, Ordering::SeqCst) == 0);
        then.status(429)
            .header("Retry-After", "0")
            .json_body(json!({"error": "rate limited"}));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-types");
        then.status(200).json_body(json!([]));
    });
    client(&server)
        .get_instance_types()
        .await
        .expect("types after 429");
}

#[tokio::test]
async fn client_refreshes_before_expiry_leeway() {
    let server = MockServer::start();
    let token = token_ok(&server, 1);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instance-types");
        then.status(200).json_body(json!([]));
    });
    let c = client(&server);
    c.get_instance_types().await.expect("first");
    c.get_instance_types().await.expect("second");
    assert!(
        token.calls() >= 2,
        "pre-expiry leeway should refresh (hits={})",
        token.calls()
    );
}

#[tokio::test]
async fn client_create_parses_bare_uuid_and_object() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    let uuid = "d332d397-f4e7-4b1b-ba61-da9333b5900e";
    server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(202).body(uuid);
    });
    let created = client(&server)
        .create_instance(json!({"instance_type": "gpu-l4"}))
        .await
        .expect("uuid");
    assert_eq!(created.instance_id_value(), Some(uuid));

    let server2 = MockServer::start();
    token_ok(&server2, 3600);
    server2.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200)
            .json_body(json!({"id": "inst-obj", "status": "pending"}));
    });
    let obj = client(&server2)
        .create_instance(json!({"instance_type": "gpu-l4"}))
        .await
        .expect("object");
    assert_eq!(obj.instance_id_value(), Some("inst-obj"));
}

#[tokio::test]
async fn manager_create_waits_for_enroll_and_tags() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    let confirm = stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    let post = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/instances")
            .json_body_includes(r#"{"startup_script_id":"script-1"}"#);
        then.status(200).json_body(json!({
            "id": "inst-new",
            "status": "pending",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-new");
        then.status(200).json_body(json!({
            "id": "inst-new",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "os_volume_id": "vol-new",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, fs) = manager(&server, false);
    persist_test_enroll(&fs, "verda-inst-new", &server.base_url());
    let out = mgr.create_additional().await.expect("create");
    assert_eq!(out["status"], "created");
    assert_eq!(out["enroll"], "ok");
    assert!(confirm.calls() >= 1, "confirm_availability must run");
    assert!(post.calls() >= 1, "create POST must run after confirm");
}

#[tokio::test]
async fn manager_creates_named_startup_script_when_missing() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog_core(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/scripts");
        then.status(200).json_body(json!([]));
    });
    let created_script = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/scripts")
            .json_body_includes(r#"{"name":"ollama-router-agent-init"}"#);
        then.status(200).body("script-new");
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    let post = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/instances")
            .json_body_includes(r#"{"startup_script_id":"script-new"}"#);
        then.status(200).json_body(json!({
            "id": "inst-script",
            "status": "running",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-script");
        then.status(200).json_body(json!({
            "id": "inst-script",
            "status": "running",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, fs) = manager(&server, false);
    persist_test_enroll(&fs, "verda-inst-script", &server.base_url());
    let out = mgr.create_additional().await.expect("create");
    assert_eq!(out["status"], "created");
    assert!(created_script.calls() >= 1);
    assert!(post.calls() >= 1);
}

#[tokio::test]
async fn manager_adopt_does_not_recreate_or_attach_startup_script() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200)
            .json_body(json!([owned_instance("inst-1")]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(500);
    });
    let scripts = server.mock(|when, then| {
        when.method(POST).path("/v1/scripts");
        then.status(500);
    });
    let (mgr, _, fs) = manager(&server, false);
    persist_test_enroll(&fs, "verda-inst-1", &server.base_url());
    let out = mgr.ensure(true).await.expect("ensure");
    assert_eq!(out["status"], "adopted");
    assert_eq!(out["enroll"], "ok");
    assert_eq!(post.calls(), 0, "adopt must not POST /v1/instances");
    assert_eq!(scripts.calls(), 0, "adopt must not create a startup script");
}

#[tokio::test]
async fn manager_adopt_waits_for_enroll_and_tags() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200)
            .json_body(json!([owned_instance("inst-1")]));
    });
    let (mgr, _, fs) = manager(&server, false);
    persist_test_enroll(&fs, "verda-inst-1", &server.base_url());
    let out = mgr.ensure(true).await.expect("ensure");
    assert_eq!(out["status"], "adopted");
    assert_eq!(out["enroll"], "ok");
}

#[tokio::test]
async fn manager_fail_does_not_set_public_url() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({
            "id": "inst-fail",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "os_volume_id": "vol-fail",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-fail");
        then.status(200).json_body(json!({
            "id": "inst-fail",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "os_volume_id": "vol-fail",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(PUT).path("/v1/instances");
        then.status(204);
    });
    let (mgr, registry, fs) = manager_with(&server, |c| {
        c.verda.auto_scale = false;
        c.verda.create_timeout_seconds = 1.0;
    });
    let out = mgr.create_additional().await.expect("create");
    assert_eq!(out["enroll"], "fail");
    assert_eq!(out["detail"], "enroll_timeout");
    let nid = NodeId::parse("verda-inst-fail").unwrap();
    let url = registry.node_config(&nid).and_then(|n| n.url);
    assert!(
        url.is_none(),
        "failed enroll must not publish a routing URL: {url:?}"
    );
    assert!(
        fs.list_verda_nodes()
            .unwrap()
            .contains_key("verda-inst-fail"),
        "enroll timeout must keep FleetState ownership"
    );
}

#[tokio::test]
async fn manager_destroy_permanent_and_idempotent() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200)
            .json_body(json!([owned_instance("inst-1")]));
    });
    let deleted = server.mock(|when, then| {
        when.method(PUT)
            .path("/v1/instances")
            .json_body_includes(r#"{"action":"delete","delete_permanently":true}"#);
        then.status(204);
    });
    let (mgr, _, fs) = manager(&server, false);
    let iid = VerdaInstanceId::parse("inst-1").unwrap();
    fs.persist_verda_node(
        "verda-inst-1",
        VerdaNodePersist {
            url: "",
            instance_id: &iid,
            location: "HEL",
            instance_type: "gpu-l4",
            os_volume_id: Some("vol-1"),
            spot_price_per_hour: None,
            hostname: None,
        },
    )
    .unwrap();
    let out = mgr.destroy_all_owned().await;
    assert_eq!(out["deleted"], json!(["inst-1"]));
    deleted.assert();

    let server404 = MockServer::start();
    token_ok(&server404, 3600);
    server404.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    server404.mock(|when, then| {
        when.method(PUT).path("/v1/instances");
        then.status(404).json_body(json!({"error": "not found"}));
    });
    let (mgr, _, fs) = manager(&server404, false);
    fs.persist_verda_node(
        "verda-inst-1",
        VerdaNodePersist {
            url: "",
            instance_id: &iid,
            location: "HEL",
            instance_type: "gpu-l4",
            os_volume_id: None,
            spot_price_per_hour: None,
            hostname: None,
        },
    )
    .unwrap();
    let out = mgr.destroy_all_owned().await;
    assert_eq!(out["failed"], json!([]));
    assert!(fs.list_verda_nodes().unwrap().is_empty());
}

#[tokio::test]
async fn manager_failed_destroy_retains_fleet_state() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    server.mock(|when, then| {
        when.method(PUT).path("/v1/instances");
        then.status(500).json_body(json!({"error": "busy"}));
    });
    let (mgr, _, fs) = manager(&server, false);
    let iid = VerdaInstanceId::parse("inst-keep").unwrap();
    fs.persist_verda_node(
        "verda-inst-keep",
        VerdaNodePersist {
            url: "http://127.0.0.1:41990",
            instance_id: &iid,
            location: "HEL",
            instance_type: "gpu-l4",
            os_volume_id: None,
            spot_price_per_hour: None,
            hostname: None,
        },
    )
    .unwrap();
    let out = mgr.destroy_all_owned().await;
    assert_eq!(out["failed"], json!(["inst-keep"]));
    assert!(fs
        .list_verda_nodes()
        .unwrap()
        .contains_key("verda-inst-keep"));
}

#[tokio::test]
async fn reconcile_adopts_orphan_when_auto_scale_false() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200)
            .json_body(json!([owned_instance("inst-9")]));
    });
    let (mgr, registry, fs) = manager(&server, false);
    persist_test_enroll(&fs, "verda-inst-9", &server.base_url());
    mgr.reconcile().await;
    let nid = NodeId::parse("verda-inst-9").unwrap();
    assert!(registry.get(&nid).is_some());
}

#[tokio::test]
async fn demand_scale_up_does_not_block() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({
            "id": "inst-d",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-d");
        then.status(200).json_body(json!({
            "id": "inst-d",
            "status": "running",
            "ip_address": "203.0.113.10",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, _) = manager(&server, true);
    let start = Instant::now();
    mgr.request_scale_up(RoutingError::NoHealthy);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "demand scale-up must not block the caller"
    );
}

#[test]
fn instance_ignores_unknown_fields() {
    let raw = r#"{"id":"x","status":"running","future_column":true,"ip":"1.2.3.4"}"#;
    let inst: Instance = serde_json::from_str(raw).expect("extras");
    assert_eq!(inst.instance_id_value(), Some("x"));
}

#[test]
fn startup_script_ignores_unknown_fields() {
    let raw =
        r#"{"id":"script-9","name":"ollama-router-agent-init","script":"true","future":true}"#;
    let script: StartupScript = serde_json::from_str(raw).expect("extras");
    assert_eq!(script.script_key(), Some("script-9"));
    assert_eq!(script.name.as_deref(), Some("ollama-router-agent-init"));
}

#[test]
fn instance_id_from_id_or_instance_id_field() {
    let from_id: Instance = serde_json::from_str(r#"{"id":"from-id"}"#).unwrap();
    assert_eq!(from_id.instance_id_value(), Some("from-id"));
    let from_alias: Instance = serde_json::from_str(r#"{"instance_id":"from-alias"}"#).unwrap();
    assert_eq!(from_alias.instance_id_value(), Some("from-alias"));
    let both: Instance =
        serde_json::from_str(r#"{"id":"primary","instance_id":"secondary"}"#).unwrap();
    assert_eq!(both.instance_id_value(), Some("primary"));
}

#[test]
fn client_missing_credentials_fails_closed() {
    let config = VerdaConfig {
        client_id_env: "OLLAMA_ROUTER_TEST_MISSING_VERDA_ID".into(),
        client_secret_env: "OLLAMA_ROUTER_TEST_MISSING_VERDA_SECRET".into(),
        ..VerdaConfig::default()
    };
    let err = match VerdaClient::new(config) {
        Err(err) => err,
        Ok(_) => panic!("missing creds must fail closed"),
    };
    match err {
        crate::client::VerdaError::Auth(msg) => {
            assert!(
                msg.contains("OLLAMA_ROUTER_TEST_MISSING_VERDA_ID")
                    || msg.contains("credentials missing"),
                "{msg}"
            );
        }
        other => panic!("expected Auth, got {other}"),
    }
}

#[tokio::test]
async fn create_additional_does_not_adopt_running_instance() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200)
            .json_body(json!([owned_instance("inst-running")]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({
            "id": "inst-extra",
            "status": "running",
            "ip_address": "203.0.113.11",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-extra");
        then.status(200).json_body(json!({
            "id": "inst-extra",
            "status": "running",
            "ip_address": "203.0.113.11",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, fs) = manager(&server, true);
    persist_test_enroll(&fs, "verda-inst-extra", &server.base_url());
    let out = mgr.create_additional().await.expect("create");
    assert_eq!(out["status"], "created");
    assert_eq!(out["instance_id"], "inst-extra");
    assert!(post.calls() >= 1);
}

#[tokio::test]
async fn illumination_managed_by_is_not_owned() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    stub_tags(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([{
            "id": "inst-old",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": "illumination-ollama-router"}],
        }]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({
            "id": "inst-new",
            "status": "running",
            "ip_address": "203.0.113.11",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-new");
        then.status(200).json_body(json!({
            "id": "inst-new",
            "status": "running",
            "ip_address": "203.0.113.11",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, fs) = manager(&server, true);
    persist_test_enroll(&fs, "verda-inst-new", &server.base_url());
    let out = mgr.ensure(true).await.expect("ensure");
    assert_eq!(out["status"], "created");
    assert!(post.calls() >= 1);
}

#[tokio::test]
async fn demand_scale_up_coalesces_create_additional() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances");
        then.status(200).json_body(json!([]));
    });
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({
            "id": "inst-d",
            "status": "running",
            "ip_address": "203.0.113.10",
            "location_code": "HEL",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/v1/instances/inst-d");
        then.status(200).json_body(json!({
            "id": "inst-d",
            "status": "running",
            "ip_address": "203.0.113.10",
            "instance_type": "gpu-l4",
            "tags": [{"key": "managed_by", "value": MANAGED_BY}],
        }));
    });
    let (mgr, _, _) = manager(&server, true);
    mgr.request_scale_up(RoutingError::NoHealthy);
    mgr.request_scale_up(RoutingError::Saturated);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(post.calls(), 1, "coalesced demand must create once");
}

#[tokio::test]
async fn demand_scale_up_respects_max_instances() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({"id": "should-not"}));
    });
    let (mgr, registry, _) = manager_with(&server, |c| {
        c.verda.auto_scale = true;
        c.verda.auto_scale_max_instances = 1;
    });
    registry.upsert_verda(NodeConfig {
        id: NodeId::parse("verda-already").unwrap(),
        url: None,
        capacity_url: None,
        labels: vec!["gpu".into(), "verda".into(), "spot".into()],
        static_capacity: Capacity::default(),
        max_inflight: None,
    });
    mgr.request_scale_up(RoutingError::Saturated);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(post.calls(), 0);
}

#[tokio::test]
async fn demand_scale_up_skips_when_auto_scale_false() {
    let server = MockServer::start();
    token_ok(&server, 3600);
    stub_catalog(&server);
    let post = server.mock(|when, then| {
        when.method(POST).path("/v1/instances");
        then.status(200).json_body(json!({"id": "should-not"}));
    });
    let (mgr, _, _) = manager(&server, false);
    mgr.request_scale_up(RoutingError::NoHealthy);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(post.calls(), 0);
}

#[test]
fn forbidden_provider_symbols_absent() {
    let sources = [
        include_str!("manager.rs"),
        include_str!("client.rs"),
        include_str!("selector.rs"),
        include_str!("keys.rs"),
        include_str!("images.rs"),
        include_str!("types.rs"),
        include_str!("lib.rs"),
        include_str!("../../ollama-router-core/src/cloud/mod.rs"),
    ];
    for src in sources {
        let lower = src.to_ascii_lowercase();
        assert!(!lower.contains("runpod"), "runpod must not appear");
        assert!(!lower.contains("thunder"), "thunder must not appear");
        assert!(
            !src.contains("illumination-ollama-router"),
            "must not tag instances illumination-ollama-router"
        );
    }
}
