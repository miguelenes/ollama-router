//! Store, recovery, placement skip, and pull-slot tests.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use serde_json::json;

use crate::config::{Capacity, NodeConfig, RouterConfig};
use crate::fleet::{NodeId, Registry};
use crate::jobs::store::unix_now;
use crate::jobs::{
    Job, JobId, JobKind, JobStatus, JobStore, JobTarget, OrchestratorError, PullOrchestrator,
    TargetStatus,
};
use crate::routing::{TargetSpec, TARGET_ALL};
use tokio_util::sync::CancellationToken;

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("node id")
}

fn node(id: &str, url: &str, vram: f64, gpus: u32) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: Some(url.trim_end_matches('/').to_string()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: Some(vram),
            ram_gb: Some(32.0),
            gpus: Some(gpus),
            cpu_cores: Some(8),
        },
        max_inflight: None,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .expect("client")
}

fn tags_body(models: &[&str]) -> String {
    let models: Vec<_> = models.iter().map(|name| json!({"name": name})).collect();
    json!({"models": models}).to_string()
}

#[test]
fn store_strips_detail_and_secret_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ops.sqlite3");
    let store = JobStore::open(&path).expect("open");
    let id = JobId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id");
    let mut targets = BTreeMap::new();
    targets.insert(
        "node-a:safe-model:latest".into(),
        JobTarget {
            node: "node-a".into(),
            model: "safe-model:latest".into(),
            status: TargetStatus::Failed,
            detail: Some("upstream response with sensitive content".into()),
        },
    );
    store
        .save(&Job {
            id,
            kind: JobKind::Pull,
            status: JobStatus::Running,
            created_at: 1.0,
            finished_at: None,
            models: vec!["safe-model:latest".into()],
            nodes: vec!["node-a".into()],
            targets,
        })
        .expect("save");

    let restored = JobStore::open(&path).expect("reopen").load().expect("load");
    let job = restored.get(&id).expect("job");
    assert!(job.targets["node-a:safe-model:latest"].detail.is_none());

    let conn = rusqlite::Connection::open(&path).expect("sqlite");
    let json: String = conn
        .query_row("SELECT targets_json FROM model_operations", [], |row| {
            row.get(0)
        })
        .expect("targets_json");
    assert!(
        !json.contains("sensitive content"),
        "detail leaked into sqlite: {json}"
    );
}

#[test]
fn store_round_trips_cancelled_and_unknown_as_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ops.sqlite3");
    let store = JobStore::open(&path).expect("open");
    let id = JobId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id");
    let mut targets = BTreeMap::new();
    targets.insert(
        "node-a:m:latest".into(),
        JobTarget {
            node: "node-a".into(),
            model: "m:latest".into(),
            status: TargetStatus::Cancelled,
            detail: None,
        },
    );
    store
        .save(&Job {
            id,
            kind: JobKind::Pull,
            status: JobStatus::Failed,
            created_at: 1.0,
            finished_at: Some(2.0),
            models: vec!["m:latest".into()],
            nodes: vec!["node-a".into()],
            targets,
        })
        .expect("save");

    let restored = JobStore::open(&path).expect("reopen").load().expect("load");
    let job = restored.get(&id).expect("job");
    assert_eq!(
        job.targets["node-a:m:latest"].status,
        TargetStatus::Cancelled
    );

    let conn = rusqlite::Connection::open(&path).expect("sqlite");
    conn.execute(
        "UPDATE model_operations SET targets_json = ?1 WHERE id = ?2",
        rusqlite::params![
            r#"{"node-a:m:latest":{"node":"node-a","model":"m:latest","status":"future_status"}}"#,
            id.to_string()
        ],
    )
    .expect("update");
    let restored = JobStore::open(&path).expect("reopen").load().expect("load");
    let job = restored.get(&id).expect("job");
    assert_eq!(job.targets["node-a:m:latest"].status, TargetStatus::Failed);
}

#[test]
fn cancelled_target_is_failure_like_in_summary() {
    let mut targets = BTreeMap::new();
    targets.insert(
        "n:m".into(),
        JobTarget {
            node: "n".into(),
            model: "m".into(),
            status: TargetStatus::Cancelled,
            detail: None,
        },
    );
    let job = Job {
        id: JobId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id"),
        kind: JobKind::Pull,
        status: JobStatus::Running,
        created_at: 1.0,
        finished_at: None,
        models: vec!["m".into()],
        nodes: vec!["n".into()],
        targets,
    };
    assert_eq!(job.summarize_status(), JobStatus::Failed);
    assert!(!TargetStatus::Cancelled.is_success_like());
    assert!(!TargetStatus::Cancelled.is_incomplete());
}

#[tokio::test]
async fn save_async_failure_omits_target_detail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ops.sqlite3");
    let store = JobStore::open(&path).expect("open");
    store.drop_table_for_tests().expect("drop");
    let secret = "upstream response with sensitive content";
    let id = JobId::parse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id");
    let mut targets = BTreeMap::new();
    targets.insert(
        "node-a:safe-model:latest".into(),
        JobTarget {
            node: "node-a".into(),
            model: "safe-model:latest".into(),
            status: TargetStatus::Failed,
            detail: Some(secret.into()),
        },
    );
    let err = store
        .save_async(&Job {
            id,
            kind: JobKind::Pull,
            status: JobStatus::Failed,
            created_at: 1.0,
            finished_at: Some(2.0),
            models: vec!["safe-model:latest".into()],
            nodes: vec!["node-a".into()],
            targets,
        })
        .await
        .expect_err("save should fail");
    let msg = err.to_string();
    assert!(!msg.contains(secret), "persist error leaked detail: {msg}");
}

#[tokio::test]
async fn recovery_already_present_does_not_repull() {
    let server = MockServer::start();
    let tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&["moondream"]));
    });
    let pulls = server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("ops.sqlite3");
    let mut config = RouterConfig {
        nodes: vec![node("node-c", &server.base_url(), 24.0, 1)],
        job_store_path: Some(store_path.to_string_lossy().into()),
        ..RouterConfig::default()
    };
    config.jobs_retention_seconds = 3600;

    let store = JobStore::open(&store_path).expect("store");
    let id = JobId::parse("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee").expect("id");
    let mut targets = BTreeMap::new();
    targets.insert(
        "node-c:moondream".into(),
        JobTarget::new("node-c", "moondream"),
    );
    targets.get_mut("node-c:moondream").expect("t").status = TargetStatus::Running;
    store
        .save(&Job {
            id,
            kind: JobKind::Pull,
            status: JobStatus::Running,
            created_at: unix_now(),
            finished_at: None,
            models: vec!["moondream".into()],
            nodes: vec!["node-c".into()],
            targets,
        })
        .expect("save");

    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("node-c"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let recovered = orch.recover_incomplete_jobs().await;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, id);
    assert_eq!(recovered[0].status, JobStatus::Success);
    assert_eq!(
        recovered[0].targets["node-c:moondream"].status,
        TargetStatus::AlreadyPresent
    );
    assert_eq!(pulls.calls(), 0);
    assert!(tags.calls() >= 1);
}

#[tokio::test]
async fn restart_dedupes_interrupted_ensure() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_millis(400))
            .body("{\"status\":\"success\"}\n");
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let store_path = dir.path().join("ops.sqlite3");
    let config = RouterConfig {
        nodes: vec![node("node-c", &server.base_url(), 24.0, 1)],
        job_store_path: Some(store_path.to_string_lossy().into()),
        jobs_retention_seconds: 3600,
        ..RouterConfig::default()
    };

    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("node-c"));
    let first =
        PullOrchestrator::new(Arc::new(config.clone()), client(), Some(registry)).expect("first");
    let job = first
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("node-c")]),
            false,
            false,
        )
        .await
        .expect("start");
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(first);

    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("node-c"));
    let second = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("second");
    let deduped = second
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("node-c")]),
            false,
            false,
        )
        .await
        .expect("dedupe");
    assert_eq!(deduped.id, job.id);
    let finished = second.wait_job(&deduped.id).await;
    assert_eq!(finished.status, JobStatus::Success);
}

#[tokio::test]
async fn hash_all_large_skips_cpu_capacity() {
    let gpu = MockServer::start();
    let cpu = MockServer::start();
    gpu.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let gpu_pull = gpu.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });
    cpu.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let cpu_pull = cpu.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![
            node("gpu", &gpu.base_url(), 80.0, 1),
            node("cpu", &cpu.base_url(), 0.0, 0),
        ],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    registry.set_healthy(&nid("cpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["llama3.1:70b".into()],
            TargetSpec::parse_one(Some(TARGET_ALL)).expect("all"),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert_eq!(
        done.targets["cpu:llama3.1:70b"].status,
        TargetStatus::SkippedCapacity
    );
    assert_eq!(
        done.targets["gpu:llama3.1:70b"].status,
        TargetStatus::Success
    );
    assert_eq!(cpu_pull.calls(), 0);
    assert_eq!(gpu_pull.calls(), 1);
}

#[tokio::test]
async fn star_selects_cpu_for_embed() {
    let cpu = MockServer::start();
    cpu.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let pull = cpu.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("cpu", &cpu.base_url(), 0.0, 0)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("cpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["qwen3-embedding:8b".into()],
            TargetSpec::Placement,
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert_eq!(
        done.targets["cpu:qwen3-embedding:8b"].status,
        TargetStatus::Success
    );
    assert_eq!(pull.calls(), 1);
}

#[tokio::test]
async fn max_pulls_per_node_serializes_two_models() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_millis(200))
            .body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        max_pulls_per_node: 1,
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let started = Instant::now();
    let job = orch
        .start_ensure(
            &["moondream".into(), "llama3.2:3b".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "pulls overlapped: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn already_present_does_not_leak_join_handle() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&["moondream"]));
    });
    let pull = server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert_eq!(
        done.targets["gpu:moondream"].status,
        TargetStatus::AlreadyPresent
    );
    assert_eq!(pull.calls(), 0);
    assert_eq!(orch.job_task_count(), 0);
}

#[tokio::test]
async fn drop_aborts_running_jobs() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_secs(5))
            .body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    orch.start_ensure(
        &["moondream".into()],
        TargetSpec::Nodes(vec![nid("gpu")]),
        false,
        false,
    )
    .await
    .expect("start");
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(orch);
}

#[tokio::test]
async fn cancel_token_marks_undispatched_failed_without_panic() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let pull = server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_millis(400))
            .body("{\"status\":\"success\"}\n");
    });

    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch =
        PullOrchestrator::with_shutdown(Arc::new(config), client(), Some(registry), shutdown)
            .expect("orch");
    let job = orch
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert!(!done.status.is_incomplete());
    assert_eq!(done.targets["gpu:moondream"].status, TargetStatus::Failed);
    assert_eq!(pull.calls(), 0);
    assert_eq!(orch.job_task_count(), 0);
}

#[tokio::test]
async fn cancel_token_lets_inflight_pull_finish_or_fail_without_panic() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_millis(200))
            .body("{\"status\":\"success\"}\n");
    });

    let shutdown = CancellationToken::new();
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        max_pulls_per_node: 1,
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::with_shutdown(
        Arc::new(config),
        client(),
        Some(registry),
        shutdown.clone(),
    )
    .expect("orch");
    let job = orch
        .start_ensure(
            &["moondream".into(), "llama3.2:3b".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown.cancel();
    let done = orch.wait_job(&job.id).await;
    assert!(!done.status.is_incomplete());
    for target in done.targets.values() {
        assert!(
            !target.status.is_incomplete(),
            "{} still incomplete",
            target.status
        );
    }
    assert_eq!(orch.job_task_count(), 0);
}

#[tokio::test]
async fn running_job_dedupes_to_existing_id() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_millis(200))
            .body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let first = orch
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let second = orch
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("dedupe");
    assert_eq!(second.id, first.id);
    let done = orch.wait_job(&first.id).await;
    assert_eq!(done.status, JobStatus::Success);
}

#[tokio::test]
async fn known_low_disk_skips_pull_target() {
    use crate::capacity::CapacityReport;
    use crate::fleet::TagRecord;

    let gpu = MockServer::start();
    gpu.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let pull = gpu.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let five_gib = 5 * 1024u64 * 1024 * 1024;
    let config = RouterConfig {
        nodes: vec![
            node("gpu", &gpu.base_url(), 24.0, 1),
            node("catalog", "http://127.0.0.1:9", 24.0, 1),
        ],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    registry.set_healthy(&nid("catalog"));
    registry.update_models_from_records(
        &nid("catalog"),
        [(
            "qwen3:8b",
            TagRecord {
                digest: "aaaaaaaaaaaa".into(),
                size: Some(five_gib),
                ..TagRecord::default()
            },
        )],
    );
    let report = CapacityReport {
        vram_gb: 24.0,
        gpus: 1,
        ram_gb: 32.0,
        disk_available_gb: Some(1.0),
        ..CapacityReport::default()
    };
    registry.apply_capacity_report(&nid("gpu"), &report, None);

    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["qwen3:8b".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert_eq!(
        done.targets["gpu:qwen3:8b"].status,
        TargetStatus::SkippedDisk
    );
    assert!(done.targets["gpu:qwen3:8b"].detail.is_none());
    assert_eq!(pull.calls(), 0);
}

#[tokio::test]
async fn unknown_disk_stays_pull_eligible() {
    use crate::fleet::TagRecord;

    let gpu = MockServer::start();
    gpu.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    let pull = gpu.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200).body("{\"status\":\"success\"}\n");
    });

    let five_gib = 5 * 1024u64 * 1024 * 1024;
    let config = RouterConfig {
        nodes: vec![
            node("gpu", &gpu.base_url(), 24.0, 1),
            node("catalog", "http://127.0.0.1:9", 24.0, 1),
        ],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    registry.set_healthy(&nid("catalog"));
    registry.update_models_from_records(
        &nid("catalog"),
        [(
            "qwen3:8b",
            TagRecord {
                digest: "aaaaaaaaaaaa".into(),
                size: Some(five_gib),
                ..TagRecord::default()
            },
        )],
    );
    // disk_available_gb stays None (unknown)
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["qwen3:8b".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    assert_eq!(done.targets["gpu:qwen3:8b"].status, TargetStatus::Success);
    assert_eq!(pull.calls(), 1);
}

#[tokio::test]
async fn subscribe_job_receives_terminal_update() {
    let server = MockServer::start();
    let _tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&["tiny:1b"]));
    });
    let _pull = server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .header("content-type", "application/x-ndjson")
            .body("{\"status\":\"success\"}\n");
    });
    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["tiny:1b".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    let mut rx = orch.subscribe_job(&job.id).expect("waiter");
    let done = orch.wait_job(&job.id).await;
    assert_eq!(done.status, JobStatus::Success);
    let _ = rx.changed().await;
    assert!(!rx.borrow().status.is_incomplete());
    assert!(orch
        .subscribe_job(&JobId::parse("00000000-0000-0000-0000-000000000000").expect("id"))
        .is_none());
}

#[tokio::test]
async fn cancel_job_marks_running_pull_failed() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(tags_body(&[]));
    });
    server.mock(|when, then| {
        when.method(POST).path("/api/pull");
        then.status(200)
            .delay(Duration::from_secs(30))
            .body("{\"status\":\"success\"}\n");
    });

    let config = RouterConfig {
        nodes: vec![node("gpu", &server.base_url(), 24.0, 1)],
        ..RouterConfig::default()
    };
    let registry = Arc::new(Registry::new(&config));
    registry.set_healthy(&nid("gpu"));
    let orch = PullOrchestrator::new(Arc::new(config), client(), Some(registry)).expect("orch");
    let job = orch
        .start_ensure(
            &["moondream".into()],
            TargetSpec::Nodes(vec![nid("gpu")]),
            false,
            false,
        )
        .await
        .expect("start");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let cancelled = orch.cancel_job(&job.id).await.expect("cancel");
    assert_eq!(cancelled.status, JobStatus::Failed);
    assert!(cancelled.finished_at.is_some());
    assert!(cancelled
        .targets
        .values()
        .any(|t| t.status == TargetStatus::Cancelled));

    let conflict = orch.cancel_job(&job.id).await;
    assert!(matches!(conflict, Err(OrchestratorError::Conflict)));

    let missing = orch
        .cancel_job(&JobId::parse("00000000-0000-0000-0000-000000000001").expect("id"))
        .await;
    assert!(matches!(missing, Err(OrchestratorError::NotFound)));
}
