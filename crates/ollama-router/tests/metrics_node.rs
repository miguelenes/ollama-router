use ollama_router::http::metrics::Metrics;
use ollama_router_core::capacity::{CapacityReport, GpuBackend, GpuDetail, Pressure};
use ollama_router_core::config::{Capacity, NodeConfig, RouterConfig};
use ollama_router_core::fleet::{FleetState, NodeId, PressureLevel, Registry};

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
        ssh: None,
        provision: None,
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
}
