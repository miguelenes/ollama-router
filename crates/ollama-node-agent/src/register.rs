//! Optional authenticated heartbeat to the router. Off unless `register.url` is set.
//! Does not create fleet membership; production inventory stays fleet.yaml.

use std::time::Duration;

use crate::http::AppState;

pub fn spawn_if_configured(state: AppState) {
    let Some(url) = state
        .config
        .register
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
    else {
        return;
    };
    let interval = Duration::from_secs(state.config.register.interval_seconds.max(5));
    let token_env = state.config.register.token_env.clone();
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
            let cpu = state.cpu_usage_pct.read().ok().and_then(|slot| *slot);
            let snap = crate::collect::collect_live(&state.config, &state.ollama_listen, cpu).await;
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .use_rustls_tls()
                .build()
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            let res = client
                .post(&url)
                .bearer_auth(token)
                .json(&snap.status)
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
