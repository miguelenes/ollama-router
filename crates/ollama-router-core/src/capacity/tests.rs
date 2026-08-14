use super::*;
use crate::config::Capacity;
use httpmock::prelude::*;
use std::time::Duration;

const LINUX_FIXTURE: &str = include_str!("../../tests/fixtures/capacity-linux.json");
const ROCM_FIXTURE: &str = include_str!("../../tests/fixtures/capacity-rocm.json");

#[test]
fn bytes_to_gib_uses_1024_cubed() {
    assert!((bytes_to_gib(32 * 1024 * 1024 * 1024) - 32.0).abs() < 1e-9);
    assert!((bytes_to_gib(1024 * 1024) - (1.0 / 1024.0)).abs() < 1e-12);
}

#[test]
fn linux_fixture_deserializes_and_ignores_shape() {
    let report: CapacityReport = serde_json::from_str(LINUX_FIXTURE).expect("fixture");
    assert!((report.vram_gb - 8.0).abs() < 1e-9);
    assert_eq!(report.gpus, 1);
    assert!((report.ram_gb - 32.0).abs() < 1e-9);
    assert_eq!(report.cpu_cores, 12);
    assert_eq!(report.hostname, "test-host");
    assert!((report.vram_free_gb - 5.75).abs() < 1e-9);
    let pressure = report.pressure.expect("nested pressure");
    assert!((pressure.ram_available_gb.unwrap() - 26.5).abs() < 1e-9);
    assert_eq!(
        pressure.ram_available_source.as_deref(),
        Some("MemAvailable")
    );
}

#[test]
fn extra_json_fields_are_ignored() {
    let raw = r#"{"vram_gb":0,"gpus":0,"ram_gb":8,"cpu_cores":4,"hostname":"cpu","collected_at":"t","gpu_names":[],"agent_version":"0","gpus_detail":[],"vram_used_gb":0,"vram_free_gb":0,"future_column":true}"#;
    let report: CapacityReport = serde_json::from_str(raw).expect("extras");
    assert_eq!(report.gpus, 0);
    assert!((report.vram_gb - 0.0).abs() < 1e-9);
}

#[test]
fn merge_fills_omitted_and_caps_explicit_vram() {
    let static_cap = Capacity {
        vram_gb: Some(8.0),
        gpus: Some(0),
        ..Capacity::default()
    };
    let discovered = Capacity {
        vram_gb: Some(16.0),
        ram_gb: Some(64.0),
        gpus: Some(2),
        cpu_cores: Some(24),
    };
    let out = merge_capacity(&static_cap, Some(&discovered), None);
    assert_eq!(out.source, CapacitySource::Agent);
    assert_eq!(out.capacity.vram_gb, Some(8.0));
    assert_eq!(out.capacity.ram_gb, Some(64.0));
    assert_eq!(out.capacity.gpus, Some(0));
    assert_eq!(out.capacity.cpu_cores, Some(24));
}

#[test]
fn merge_zero_discovery_does_not_zero_explicit_positive() {
    let static_cap = Capacity {
        vram_gb: Some(4.0),
        ram_gb: Some(48.0),
        gpus: Some(1),
        cpu_cores: Some(12),
    };
    let discovered = Capacity {
        vram_gb: Some(0.0),
        ram_gb: Some(46.19),
        gpus: Some(0),
        cpu_cores: Some(24),
    };
    let out = merge_capacity(&static_cap, Some(&discovered), None);
    assert_eq!(out.capacity.vram_gb, Some(4.0));
    assert_eq!(out.capacity.gpus, Some(1));
    assert_eq!(out.capacity.ram_gb, Some(46.19));
    assert_eq!(out.source, CapacitySource::Agent);
}

#[test]
fn merge_both_absent_is_unknown() {
    let out = merge_capacity(&Capacity::default(), None, None);
    assert_eq!(out.source, CapacitySource::Unknown);
    assert_eq!(out.capacity, Capacity::default());
}

#[test]
fn merge_static_only_is_static() {
    let static_cap = Capacity {
        vram_gb: Some(8.0),
        ..Capacity::default()
    };
    let out = merge_capacity(&static_cap, None, None);
    assert_eq!(out.source, CapacitySource::Static);
    assert_eq!(out.capacity.vram_gb, Some(8.0));
}

#[test]
fn ps_lower_bound_does_not_change_source() {
    let out = merge_capacity(&Capacity::default(), None, Some(7.2));
    assert_eq!(out.capacity.vram_gb, Some(8.0));
    assert_eq!(out.capacity.gpus, Some(1));
    assert_eq!(out.source, CapacitySource::Unknown);
}

#[test]
fn ps_lower_bound_does_not_shrink_discovered_vram() {
    let discovered = Capacity {
        vram_gb: Some(12.0),
        gpus: Some(1),
        ..Capacity::default()
    };
    let out = merge_capacity(&Capacity::default(), Some(&discovered), Some(7.2));
    assert_eq!(out.capacity.vram_gb, Some(12.0));
    assert_eq!(out.capacity.gpus, Some(1));
    assert_eq!(out.source, CapacitySource::Agent);
}

#[tokio::test]
async fn client_reads_fixture_and_pressure_level() {
    let server = MockServer::start();
    let _cap = server.mock(|when, then| {
        when.method(GET).path("/v1/capacity");
        then.status(200)
            .header("content-type", "application/json")
            .body(LINUX_FIXTURE);
    });
    let _pressure = server.mock(|when, then| {
        when.method(GET).path("/v1/pressure");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"collected_at":"t","pressure_level":"ok","pressure":{"ram_available_gb":26.5}}"#);
    });
    let client = CapacityClient::new(reqwest::Client::builder().use_rustls_tls().build().unwrap());
    let target = CapacityTarget {
        capacity_url: format!("{}/v1/capacity", server.base_url()),
        pressure_url: format!("{}/v1/pressure", server.base_url()),
    };
    let probe = client
        .probe(&target, None, Duration::from_secs(2), 8 * 1024 * 1024)
        .await
        .expect("probe");
    assert!((probe.report.vram_gb - 8.0).abs() < 1e-9);
    assert_eq!(probe.pressure_level.as_deref(), Some("ok"));
}

#[tokio::test]
async fn client_soft_fails_on_http_error_without_json_feature() {
    let server = MockServer::start();
    let _cap = server.mock(|when, then| {
        when.method(GET).path("/v1/capacity");
        then.status(503).body("ignored-body");
    });
    let client = CapacityClient::new(reqwest::Client::builder().use_rustls_tls().build().unwrap());
    let target = CapacityTarget {
        capacity_url: format!("{}/v1/capacity", server.base_url()),
        pressure_url: format!("{}/v1/pressure", server.base_url()),
    };
    let err = client
        .probe(&target, None, Duration::from_secs(2), 8 * 1024 * 1024)
        .await
        .expect_err("http");
    assert_eq!(err, CapacityError::Http { status: 503 });
    assert_eq!(err.as_reason(), "http_status");
}

#[tokio::test]
async fn pressure_miss_still_returns_capacity() {
    let server = MockServer::start();
    let _cap = server.mock(|when, then| {
        when.method(GET).path("/v1/capacity");
        then.status(200)
            .header("content-type", "application/json")
            .body(LINUX_FIXTURE);
    });
    let _pressure = server.mock(|when, then| {
        when.method(GET).path("/v1/pressure");
        then.status(500);
    });
    let client = CapacityClient::new(reqwest::Client::builder().use_rustls_tls().build().unwrap());
    let base = url::Url::parse(&server.base_url()).expect("base url");
    let port = base.port().expect("mock port");
    let target = capacity_target(
        Some(&server.base_url()),
        None,
        port,
        "/v1/capacity",
        "/v1/pressure",
    )
    .expect("target");
    let probe = client
        .probe(&target, None, Duration::from_secs(2), 8 * 1024 * 1024)
        .await
        .expect("capacity ok");
    assert!(probe.pressure_level.is_none());
    assert!((probe.report.vram_gb - 8.0).abs() < 1e-9);
}

#[test]
fn rocm_fixture_deserializes_backend_and_known_flags() {
    let report: CapacityReport = serde_json::from_str(ROCM_FIXTURE).expect("rocm fixture");
    assert_eq!(report.gpu_backend, Some(GpuBackend::Rocm));
    assert_eq!(report.gpus, 1);
    assert!((report.vram_gb - 32.0).abs() < 1e-9);
    assert!((bytes_to_gib(32 * 1024 * 1024 * 1024) - 32.0).abs() < 1e-9);
    assert_eq!(report.vram_free_known, Some(true));
    assert_eq!(report.vram_used_known, Some(true));
    assert!(report.vram_free_is_known());
    assert!((report.vram_used_gb - 1.0).abs() < 1e-9);
    assert!((report.vram_free_gb - 31.0).abs() < 1e-9);
}

#[tokio::test]
async fn client_reads_rocm_fixture() {
    let server = MockServer::start();
    let _cap = server.mock(|when, then| {
        when.method(GET).path("/v1/capacity");
        then.status(200)
            .header("content-type", "application/json")
            .body(ROCM_FIXTURE);
    });
    let _pressure = server.mock(|when, then| {
        when.method(GET).path("/v1/pressure");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"collected_at":"t","pressure_level":"ok","pressure":{"ram_available_gb":48.0}}"#);
    });
    let client = CapacityClient::new(reqwest::Client::builder().use_rustls_tls().build().unwrap());
    let target = CapacityTarget {
        capacity_url: format!("{}/v1/capacity", server.base_url()),
        pressure_url: format!("{}/v1/pressure", server.base_url()),
    };
    let probe = client
        .probe(&target, None, Duration::from_secs(2), 8 * 1024 * 1024)
        .await
        .expect("probe");
    assert_eq!(probe.report.gpu_backend, Some(GpuBackend::Rocm));
    assert!(probe.report.vram_free_is_known());
    assert!((probe.report.vram_gb - 32.0).abs() < 1e-9);
    assert_eq!(probe.pressure_level.as_deref(), Some("ok"));
}
