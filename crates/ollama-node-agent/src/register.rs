//! Optional authenticated heartbeat to `POST /router/v1/nodes/enroll`.
//!
//! Off unless `register.url` or setup `--enroll-url` (state.json) is set.
//! Production inventory stays fleet.yaml + FleetState + Verda; enroll hydrates
//! reachability only. Share unique-names are sent — never zrok enable tokens,
//! never Ollama request bodies.

use std::time::Duration;

use serde::Serialize;

use crate::config::AgentConfig;
use crate::http::AppState;
use crate::setup::{ConvergeState, SetupPaths};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EnrollHeartbeat {
    pub id: String,
    pub origin: String,
    pub ollama_share_id: String,
    pub agent_share_id: String,
    pub agent_version: String,
    pub hostname: String,
}

pub fn enroll_endpoint(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.ends_with("/router/v1/nodes/enroll") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/router/v1/nodes/enroll")
    }
}

pub fn resolve_register_url(config: &AgentConfig, state: &ConvergeState) -> Option<String> {
    config
        .register
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            state
                .enroll_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        })
}

pub fn resolve_token_env(config: &AgentConfig, state: &ConvergeState) -> String {
    state
        .enroll_token_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(config.register.token_env.trim())
        .to_string()
}

pub fn enroll_heartbeat(
    config: &AgentConfig,
    state: &ConvergeState,
    hostname: &str,
    agent_version: &str,
) -> Option<EnrollHeartbeat> {
    let ollama = state
        .ollama_share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let agent = state
        .agent_share_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let origin = config.register.origin.trim();
    let origin = if origin.is_empty() { "adopt" } else { origin };
    let id = config
        .register
        .node_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(hostname);
    if id.is_empty() {
        return None;
    }
    Some(EnrollHeartbeat {
        id: id.to_string(),
        origin: origin.to_ascii_lowercase(),
        ollama_share_id: ollama.to_string(),
        agent_share_id: agent.to_string(),
        agent_version: agent_version.to_string(),
        hostname: hostname.to_string(),
    })
}

pub fn spawn_if_configured(state: AppState) {
    spawn_if_configured_with(state, SetupPaths::for_os());
}

fn spawn_if_configured_with(app: AppState, paths: SetupPaths) {
    let converge = ConvergeState::load(&paths.state);
    let Some(url) = resolve_register_url(&app.config, &converge) else {
        return;
    };
    let interval = Duration::from_secs(app.config.register.interval_seconds.max(5));
    let token_env = resolve_token_env(&app.config, &converge);
    let endpoint = enroll_endpoint(&url);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            let token = std::env::var(&token_env)
                .ok()
                .filter(|s| !s.trim().is_empty());
            let Some(token) = token else {
                tracing::warn!("register skipped: token env unset");
                continue;
            };
            let latest = ConvergeState::load(&paths.state);
            let cpu = app.cpu_usage_pct.read().ok().and_then(|slot| *slot);
            let snap = crate::collect::collect_live(&app.config, &app.ollama_listen, cpu).await;
            let Some(body) = enroll_heartbeat(
                &app.config,
                &latest,
                &snap.status.hostname,
                &snap.status.agent_version,
            ) else {
                tracing::warn!("register skipped: share ids missing");
                continue;
            };
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .use_rustls_tls()
                .build()
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            let res = client
                .post(&endpoint)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => {
                    tracing::info!("register heartbeat ok");
                }
                Ok(r) => {
                    tracing::warn!(status = r.status().as_u16(), "register heartbeat rejected");
                }
                Err(_) => {
                    tracing::warn!("register heartbeat unreachable");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::setup::{ConvergeState, STATE_SCHEMA};

    #[test]
    fn endpoint_appends_enroll_path() {
        assert_eq!(
            enroll_endpoint("http://router:11435"),
            "http://router:11435/router/v1/nodes/enroll"
        );
        assert_eq!(
            enroll_endpoint("http://router:11435/router/v1/nodes/enroll"),
            "http://router:11435/router/v1/nodes/enroll"
        );
    }

    #[test]
    fn heartbeat_includes_share_ids_not_enable_token() {
        let mut config = AgentConfig::default();
        config.register.node_id = Some("nuc".into());
        config.register.origin = "fleet".into();
        let state = ConvergeState {
            schema: STATE_SCHEMA,
            ollama_share_token: Some("share-ollama".into()),
            agent_share_token: Some("share-agent".into()),
            ..ConvergeState::default()
        };
        let body = enroll_heartbeat(&config, &state, "host", "0.1.0").unwrap();
        assert_eq!(body.id, "nuc");
        assert_eq!(body.origin, "fleet");
        assert_eq!(body.ollama_share_id, "share-ollama");
        assert_eq!(body.agent_share_id, "share-agent");
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("enable_token").is_none());
    }

    #[test]
    fn resolve_url_prefers_config_then_state() {
        let mut config = AgentConfig::default();
        let mut state = ConvergeState {
            schema: STATE_SCHEMA,
            enroll_url: Some("http://from-state".into()),
            ..ConvergeState::default()
        };
        assert_eq!(
            resolve_register_url(&config, &state).as_deref(),
            Some("http://from-state")
        );
        config.register.url = Some("http://from-config".into());
        assert_eq!(
            resolve_register_url(&config, &state).as_deref(),
            Some("http://from-config")
        );
        state.enroll_token_env = Some("CUSTOM_TOKEN".into());
        assert_eq!(resolve_token_env(&config, &state), "CUSTOM_TOKEN");
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::setup::{ConvergeState, STATE_SCHEMA};
    use httpmock::prelude::*;

    #[tokio::test]
    async fn enroll_heartbeat_posts_allowlisted_json() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/router/v1/nodes/enroll")
                .header("authorization", "Bearer secret")
                .json_body(serde_json::json!({
                    "id": "nuc",
                    "origin": "fleet",
                    "ollama_share_id": "share-ollama",
                    "agent_share_id": "share-agent",
                    "agent_version": "0.1.0",
                    "hostname": "host"
                }));
            then.status(200).json_body(serde_json::json!({"ok": true}));
        });
        let mut config = AgentConfig::default();
        config.register.node_id = Some("nuc".into());
        config.register.origin = "fleet".into();
        let state = ConvergeState {
            schema: STATE_SCHEMA,
            ollama_share_token: Some("share-ollama".into()),
            agent_share_token: Some("share-agent".into()),
            ..ConvergeState::default()
        };
        let body = enroll_heartbeat(&config, &state, "host", "0.1.0").unwrap();
        let client = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        let resp = client
            .post(enroll_endpoint(&server.base_url()))
            .bearer_auth("secret")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        mock.assert();
        assert_eq!(mock.calls(), 1);
    }
}
