use std::time::{Duration, Instant};

use ollama_router::http::metrics::Metrics;
use ollama_router_core::capacity::{CapacityReport, GpuBackend, GpuDetail, Pressure};
use ollama_router_core::cloud::FleetEvents;
use ollama_router_core::config::{Capacity, NodeConfig, RouterConfig};
use ollama_router_core::fleet::{
    CloudInstanceId, EnrollPersist, FleetState, NodeId, PressureLevel, Registry, RunpodNodePersist,
    VerdaNodePersist,
};

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("node id")
}

fn node(id: &str, vram: f64, gpus: u32) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: Some(format!("http://{id}:11434")),
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

#[test]
fn refresh_gauges_exports_ram_util_and_known_flags() {
    let dir = tempfile::tempdir().expect("tmp");
    let fleet_state = FleetState::new(dir.path().join("state.json"));
    let registry = Registry::new(&RouterConfig {
        nodes: vec![node("gpu", 8.0, 1)],
        ..Default::default()
    });
    let mut report = CapacityReport {
        vram_gb: 8.0,
        gpus: 1,
        ram_gb: 32.0,
        vram_used_gb: 8.0,
        vram_free_gb: 0.0,
        vram_free_known: Some(true),
        vram_used_known: Some(true),
        gpu_backend: Some(GpuBackend::Cuda),
        cpu_usage_pct: Some(11.0),
        ollama_running: Some(true),
        loaded_model_count: Some(1),
        ..CapacityReport::default()
    };
    report.gpus_detail.push(GpuDetail {
        index: 0,
        vram_total_gb: 8.0,
        vram_used_gb: 8.0,
        vram_free_gb: 0.0,
        utilization_gpu_pct: Some(88.0),
        vram_free_known: Some(true),
        vram_used_known: Some(true),
        util_known: Some(true),
        ..GpuDetail::default()
    });
    report.pressure = Some(Pressure {
        ram_available_gb: Some(20.0),
        ram_available_ratio: Some(0.625),
        ..Pressure::default()
    });
    registry.apply_capacity_report(&nid("gpu"), &report, Some(PressureLevel::Ok));

    let metrics = Metrics::new().expect("metrics");
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");
    assert!(body.contains("ollama_router_node_vram_free_known{node=\"gpu\"} 1"));
    assert!(body.contains("ollama_router_node_vram_free_gb{node=\"gpu\"} 0"));
    assert!(body.contains("ollama_router_node_ram_available_gb{node=\"gpu\"} 20"));
    assert!(body.contains("ollama_router_node_gpu_utilization_pct{node=\"gpu\"} 88"));
    assert!(body.contains("ollama_router_node_gpu_util_known{node=\"gpu\"} 1"));
    assert!(
        body.contains("ollama_router_node_backend_info{backend=\"cuda\",node=\"gpu\"} 1")
            || body.contains("ollama_router_node_backend_info{node=\"gpu\",backend=\"cuda\"} 1")
    );
}

#[test]
fn refresh_gauges_unknown_free_is_known_zero() {
    let dir = tempfile::tempdir().expect("tmp");
    let fleet_state = FleetState::new(dir.path().join("state.json"));
    let registry = Registry::new(&RouterConfig {
        nodes: vec![node("cpu", 0.0, 0)],
        ..Default::default()
    });
    let report = CapacityReport {
        vram_gb: 0.0,
        gpus: 0,
        ram_gb: 16.0,
        vram_free_gb: 0.0,
        vram_free_known: Some(false),
        gpu_backend: Some(GpuBackend::Cpu),
        ..CapacityReport::default()
    };
    registry.apply_capacity_report(&nid("cpu"), &report, None);
    let metrics = Metrics::new().expect("metrics");
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");
    assert!(body.contains("ollama_router_node_vram_free_known{node=\"cpu\"} 0"));
    assert!(body.contains("ollama_router_node_gpu_util_known{node=\"cpu\"} 0"));
    assert!(body.contains("ollama_router_tunnel_up{node=\"cpu\"} 0"));
}

#[test]
fn refresh_gauges_tunnel_up_when_zrok_enroll() {
    let dir = tempfile::tempdir().expect("tmp");
    let fleet_state = FleetState::new(dir.path().join("state.json"));
    fleet_state
        .persist_enroll(
            "gpu",
            EnrollPersist {
                url: "http://127.0.0.1:41990",
                capacity_url: "http://127.0.0.1:41991",
                ollama_share_id: "share-ollama",
                agent_share_id: "share-agent",
            },
        )
        .expect("enroll");
    let registry = Registry::new(&RouterConfig {
        nodes: vec![NodeConfig {
            id: nid("gpu"),
            url: Some("http://127.0.0.1:41990".into()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: Some(8.0),
                ram_gb: Some(32.0),
                gpus: Some(1),
                cpu_cores: Some(8),
            },
            max_inflight: None,
        }],
        ..Default::default()
    });
    let metrics = Metrics::new().expect("metrics");
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");
    assert!(
        body.contains("ollama_router_tunnel_up{node=\"gpu\"} 1"),
        "{body}"
    );
    assert!(
        !body.contains("share-ollama"),
        "share ids must not be metric labels: {body}"
    );
}

#[test]
fn refresh_gauges_uses_snapshot_when_lock_held_and_disk_unreadable() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("state.json");
    let fleet_state = FleetState::new(&path);
    fleet_state
        .persist_enroll(
            "gpu",
            EnrollPersist {
                url: "http://127.0.0.1:41990",
                capacity_url: "http://127.0.0.1:41991",
                ollama_share_id: "share-ollama",
                agent_share_id: "share-agent",
            },
        )
        .expect("enroll");
    std::fs::write(&path, "not-json").expect("corrupt primary");
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".bak");
    std::fs::write(&backup, "not-json").expect("corrupt backup");
    assert!(
        fleet_state.load().is_err(),
        "load must fail so scrape cannot cheat via disk"
    );
    let _lock = fleet_state.lock_exclusive().expect("lock");
    let registry = Registry::new(&RouterConfig {
        nodes: vec![NodeConfig {
            id: nid("gpu"),
            url: Some("http://127.0.0.1:41990".into()),
            capacity_url: None,
            labels: Vec::new(),
            static_capacity: Capacity {
                vram_gb: Some(8.0),
                ram_gb: Some(32.0),
                gpus: Some(1),
                cpu_cores: Some(8),
            },
            max_inflight: None,
        }],
        ..Default::default()
    });
    let metrics = Metrics::new().expect("metrics");
    let start = Instant::now();
    metrics.refresh_gauges(&registry, &fleet_state);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "scrape blocked on flock: {:?}",
        start.elapsed()
    );
    let body = metrics.encode_text().expect("encode");
    assert!(
        body.contains("ollama_router_tunnel_up{node=\"gpu\"} 1"),
        "{body}"
    );
    assert!(
        !body.contains("share-ollama"),
        "share ids must not be metric labels: {body}"
    );
}

#[test]
fn refresh_gauges_draining_includes_cordoned() {
    let dir = tempfile::tempdir().expect("tmp");
    let fleet_state = FleetState::new(dir.path().join("state.json"));
    let registry = Registry::new(&RouterConfig {
        nodes: vec![node("gpu", 8.0, 1)],
        ..Default::default()
    });
    assert!(registry.set_cordoned(&nid("gpu"), true));
    let metrics = Metrics::new().expect("metrics");
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");
    assert!(
        body.contains("ollama_router_node_draining{node=\"gpu\"} 1"),
        "{body}"
    );
    assert!(registry.set_cordoned(&nid("gpu"), false));
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");
    assert!(
        body.contains("ollama_router_node_draining{node=\"gpu\"} 0"),
        "{body}"
    );
}

#[test]
fn cloud_metrics_attribute_per_provider() {
    let dir = tempfile::tempdir().expect("tmp");
    let fleet_state = FleetState::new(dir.path().join("state.json"));
    let verda_id = CloudInstanceId::parse("i-verda-1").expect("verda id");
    let runpod_id = CloudInstanceId::parse("pod-runpod-1").expect("runpod id");
    fleet_state
        .persist_verda_node(
            "spot-verda",
            VerdaNodePersist {
                url: "http://127.0.0.1:41990",
                instance_id: &verda_id,
                location: "HEL1",
                instance_type: "gpu",
                os_volume_id: None,
                spot_price_per_hour: Some(0.42),
                hostname: None,
            },
        )
        .expect("verda persist");
    fleet_state
        .persist_runpod_node(
            "spot-runpod",
            RunpodNodePersist {
                url: "http://127.0.0.1:41991",
                pod_id: &runpod_id,
                gpu_type: "NVIDIA GeForce RTX 4090",
                data_center: Some("EU-RO-1"),
                cost_per_hour: Some(0.39),
                hostname: None,
            },
        )
        .expect("runpod persist");

    let registry = Registry::new(&RouterConfig::default());
    registry.upsert_verda(NodeConfig {
        id: nid("spot-verda"),
        url: Some("http://127.0.0.1:41990".into()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: Some(24.0),
            ram_gb: Some(32.0),
            gpus: Some(1),
            cpu_cores: Some(8),
        },
        max_inflight: None,
    });
    registry.upsert_runpod(NodeConfig {
        id: nid("spot-runpod"),
        url: Some("http://127.0.0.1:41991".into()),
        capacity_url: None,
        labels: Vec::new(),
        static_capacity: Capacity {
            vram_gb: Some(24.0),
            ram_gb: Some(32.0),
            gpus: Some(1),
            cpu_cores: Some(8),
        },
        max_inflight: None,
    });

    let metrics = Metrics::new().expect("metrics");
    metrics.cloud_event("verda", "create");
    metrics.cloud_event("runpod", "create");
    metrics.cloud_event("verda", "destroy");
    metrics.refresh_gauges(&registry, &fleet_state);
    let body = metrics.encode_text().expect("encode");

    assert!(
        body.contains("ollama_router_cloud_instances{provider=\"verda\"} 1"),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_instances{provider=\"runpod\"} 1"),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_price_per_hour{provider=\"verda\"} 0.42")
            || body.contains("ollama_router_cloud_price_per_hour{provider=\"verda\"} 0.420"),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_price_per_hour{provider=\"runpod\"} 0.39")
            || body.contains("ollama_router_cloud_price_per_hour{provider=\"runpod\"} 0.390"),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_events_total{event=\"create\",provider=\"verda\"} 1")
            || body.contains(
                "ollama_router_cloud_events_total{provider=\"verda\",event=\"create\"} 1"
            ),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_events_total{event=\"create\",provider=\"runpod\"} 1")
            || body.contains(
                "ollama_router_cloud_events_total{provider=\"runpod\",event=\"create\"} 1"
            ),
        "{body}"
    );
    assert!(
        body.contains("ollama_router_cloud_events_total{event=\"destroy\",provider=\"verda\"} 1")
            || body.contains(
                "ollama_router_cloud_events_total{provider=\"verda\",event=\"destroy\"} 1"
            ),
        "{body}"
    );
    assert!(
        body.contains("origin=\"runpod\"") && body.contains("ollama_router_node_info{"),
        "node_info must export runpod origin: {body}"
    );
    assert!(
        !body.contains("ollama_router_verda_"),
        "legacy verda series must be gone: {body}"
    );
}
