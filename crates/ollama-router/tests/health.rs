//! httpmock coverage for per-node health probes and the warm-keeper.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use httpmock::prelude::*;
use ollama_router::health::run as run_health;
use ollama_router::http::{make_app, AppState};
use ollama_router::warm::run as run_warm;
use ollama_router_core::config::{
    Capacity, HealthConfig, ModelTier, NodeConfig, PolicyConfig, RouterConfig,
};
use ollama_router_core::fleet::NodeId;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn nid(id: &str) -> NodeId {
    NodeId::parse(id).expect("node id")
}

fn spawn_health(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_health(state, CancellationToken::new()))
}

fn spawn_warm(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_warm(state, CancellationToken::new()))
}

fn node(id: &str, url: Option<&str>, vram: f64, gpus: u32) -> NodeConfig {
    NodeConfig {
        id: nid(id),
        url: url.map(|u| u.trim_end_matches('/').to_string()),
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

fn fast_health() -> HealthConfig {
    HealthConfig {
        interval_seconds: 0.05,
        probe_timeout_seconds: 0.4,
        fail_streak_threshold: 2,
        success_threshold: 1,
        backoff_max_seconds: 1.0,
        probe_jitter_ratio: 0.0,
        capacity_probe_timeout_seconds: 0.4,
        ..HealthConfig::default()
    }
}

fn state_from(mut config: RouterConfig) -> AppState {
    config.health = fast_health();
    AppState::from_config(config).expect("state")
}

async fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) {
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for condition");
}

#[tokio::test]
async fn no_url_marks_unhealthy() {
    let state = state_from(RouterConfig {
        nodes: vec![node("no-url", None, 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(2), || {
        state
            .registry
            .get(&nid("no-url"))
            .is_some_and(|n| n.unhealthy_reason.as_deref() == Some("no_url"))
    })
    .await;
    handle.abort();
}

#[tokio::test]
async fn public_url_blocked_without_http() {
    let state = state_from(RouterConfig {
        nodes: vec![node("public", Some("http://8.8.8.8:11434"), 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(2), || {
        state
            .registry
            .get(&nid("public"))
            .is_some_and(|n| n.unhealthy_reason.as_deref() == Some("public_url_blocked"))
    })
    .await;
    handle.abort();
}

#[tokio::test]
async fn public_ipv6_blocked_without_http() {
    let state = state_from(RouterConfig {
        nodes: vec![node(
            "public",
            Some("http://[2606:4700:4700::1111]:11434"),
            8.0,
            1,
        )],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(2), || {
        state
            .registry
            .get(&nid("public"))
            .is_some_and(|n| n.unhealthy_reason.as_deref() == Some("public_url_blocked"))
    })
    .await;
    handle.abort();
}

#[tokio::test]
async fn public_share_hostname_blocked_without_http() {
    let state = state_from(RouterConfig {
        nodes: vec![node("public", Some("https://abc.share.zrok.io"), 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(2), || {
        state
            .registry
            .get(&nid("public"))
            .is_some_and(|n| n.unhealthy_reason.as_deref() == Some("public_url_blocked"))
    })
    .await;
    handle.abort();
}

#[tokio::test]
async fn fail_streak_benches_node() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(500);
    });
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || {
        state
            .registry
            .get(&nid("gpu"))
            .is_some_and(|n| !n.healthy && n.fail_streak >= 2)
    })
    .await;
    handle.abort();
    assert!(mock.calls() >= 2);
}

#[tokio::test]
async fn ps_and_capacity_soft_fail_keep_healthy() {
    let server = MockServer::start();
    let _tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"llama3.2:3b"}]}"#);
    });
    let _ps = server.mock(|when, then| {
        when.method(GET).path("/api/ps");
        then.status(500);
    });
    let _cap = server.mock(|when, then| {
        when.method(GET).path("/v1/capacity");
        then.status(503);
    });
    let mut config = RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    };
    config.health = fast_health();
    config.health.capacity_probe_port =
        url::Url::parse(&server.base_url()).unwrap().port().unwrap();
    let state = AppState::from_config(config).expect("state");
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || {
        state.registry.get(&nid("gpu")).is_some_and(|n| n.healthy)
    })
    .await;
    wait_until(Duration::from_secs(2), || {
        state
            .registry
            .get(&nid("gpu"))
            .is_some_and(|n| n.capacity_error.is_some())
    })
    .await;
    let snap = state.registry.get(&nid("gpu")).unwrap();
    assert!(snap.healthy);
    assert_eq!(snap.capacity_error.as_deref(), Some("http_status"));
    handle.abort();
}

#[tokio::test]
async fn tags_cache_does_not_fan_out() {
    let server = MockServer::start();
    let tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"llama3.2:3b"}]}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || {
        state.registry.get(&nid("gpu")).is_some_and(|n| n.healthy)
    })
    .await;
    handle.abort();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let hits_before = tags.calls();
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/tags")
        .body(Body::empty())
        .unwrap();
    let response = make_app(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = response.into_body().collect().await.unwrap();
    assert_eq!(tags.calls(), hits_before);
}

#[tokio::test]
async fn remove_permanent_aborts_probe_task() {
    let server = MockServer::start();
    let tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    });
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || tags.calls() >= 1).await;
    state.registry.apply_permanent_inventory(&[]);
    wait_until(Duration::from_secs(1), || {
        state.registry.get(&nid("gpu")).is_none()
    })
    .await;
    let hits = tags.calls();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(tags.calls(), hits);
    handle.abort();
}

#[tokio::test]
async fn warm_occupancy_does_not_set_idle() {
    let server = MockServer::start();
    let _tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"llama3.2:3b"}]}"#);
    });
    let generate = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"model":"llama3.2:3b","done":true}"#);
    });
    let mut config = RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 24.0, 1)],
        desired_model_tiers: vec![ModelTier {
            models: vec!["llama3.2:3b".into()],
            min_vram_gb: 0.0,
        }],
        policy: PolicyConfig {
            model_warm_enabled: true,
            model_warm_interval_seconds: 0.05,
            model_warm_cooldown_seconds: 0.05,
            model_warm_min_free_vram_gb: 0.0,
            ..PolicyConfig::default()
        },
        ..RouterConfig::default()
    };
    config.health = fast_health();
    let state = AppState::from_config(config).expect("state");
    state.registry.set_healthy(&nid("gpu"));
    state.registry.update_models(&nid("gpu"), ["llama3.2:3b"]);
    let health = spawn_health(state.clone());
    let warm = spawn_warm(state.clone());
    wait_until(Duration::from_secs(3), || generate.calls() >= 1).await;
    assert!(state.registry.last_client_request_at(&nid("gpu")).is_none());
    wait_until(Duration::from_secs(1), || {
        state.registry.inflight(&nid("gpu")) == 0
    })
    .await;
    health.abort();
    warm.abort();
}

#[tokio::test]
async fn warm_skips_cpu_for_gpu_class_tier() {
    let server = MockServer::start();
    let generate = server.mock(|when, then| {
        when.method(POST).path("/api/generate");
        then.status(200).body("{}");
    });
    let mut config = RouterConfig {
        nodes: vec![node("cpu", Some(&server.base_url()), 0.0, 0)],
        desired_model_tiers: vec![ModelTier {
            models: vec!["llama3.1:8b".into()],
            min_vram_gb: 12.0,
        }],
        policy: PolicyConfig {
            model_warm_enabled: true,
            model_warm_interval_seconds: 0.05,
            model_warm_cooldown_seconds: 0.05,
            model_warm_min_free_vram_gb: 0.0,
            ..PolicyConfig::default()
        },
        ..RouterConfig::default()
    };
    config.health = fast_health();
    let state = AppState::from_config(config).expect("state");
    state.registry.set_healthy(&nid("cpu"));
    state.registry.update_models(&nid("cpu"), ["llama3.1:8b"]);
    let warm = spawn_warm(state.clone());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(generate.calls(), 0);
    assert!(state.registry.last_client_request_at(&nid("cpu")).is_none());
    warm.abort();
}

#[tokio::test]
async fn oversized_tags_does_not_empty_models() {
    let server = MockServer::start();
    let tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .header("content-length", "1000000")
            .body(r#"{"models":[{"name":"should-not-replace"}]}"#);
    });
    let mut config = RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    };
    config.health = HealthConfig {
        max_probe_body_bytes: 8,
        capacity_probe_enabled: false,
        ..fast_health()
    };
    let state = AppState::from_config(config).expect("state");
    state.registry.set_healthy(&nid("gpu"));
    state.registry.update_models(&nid("gpu"), ["llama3.2:3b"]);
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || {
        state
            .registry
            .get(&nid("gpu"))
            .is_some_and(|n| !n.healthy && n.fail_streak >= 2)
    })
    .await;
    let snap = state.registry.get(&nid("gpu")).unwrap();
    assert!(snap.has_model("llama3.2:3b"));
    assert!(!snap.has_model("should-not-replace"));
    handle.abort();
    assert!(tags.calls() >= 2);
}

#[tokio::test]
async fn oversized_ps_does_not_clear_loaded_or_health() {
    let server = MockServer::start();
    let _tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[{"name":"llama3.2:3b"}]}"#);
    });
    let ps = server.mock(|when, then| {
        when.method(GET).path("/api/ps");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"models":[],"pad":"{}"}}"#, "x".repeat(200)));
    });
    let mut config = RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    };
    config.health = HealthConfig {
        max_probe_body_bytes: 64,
        capacity_probe_enabled: false,
        ..fast_health()
    };
    let state = AppState::from_config(config).expect("state");
    state.registry.set_healthy(&nid("gpu"));
    state.registry.update_models(&nid("gpu"), ["llama3.2:3b"]);
    state
        .registry
        .update_ps_state(&nid("gpu"), ["llama3.2:3b"], Some(3.0));
    let handle = spawn_health(state.clone());
    wait_until(Duration::from_secs(3), || ps.calls() >= 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snap = state.registry.get(&nid("gpu")).unwrap();
    assert!(snap.healthy);
    assert!(snap.has_model_loaded("llama3.2:3b"));
    handle.abort();
}

#[tokio::test]
async fn cancel_stops_health_supervisor() {
    let server = MockServer::start();
    let tags = server.mock(|when, then| {
        when.method(GET).path("/api/tags");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"models":[]}"#);
    });
    let state = state_from(RouterConfig {
        nodes: vec![node("gpu", Some(&server.base_url()), 8.0, 1)],
        ..RouterConfig::default()
    });
    let token = CancellationToken::new();
    let handle = tokio::spawn(run_health(state.clone(), token.clone()));
    wait_until(Duration::from_secs(3), || tags.calls() >= 1).await;
    token.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("health supervisor should stop after cancel")
        .expect("join");
    let hits = tags.calls();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(tags.calls(), hits);
}
