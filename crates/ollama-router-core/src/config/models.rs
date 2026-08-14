//! Serde config models (YAML tunables + fleet.yaml nodes).

use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fleet::ids::NodeId;

use super::error::ConfigError;

pub(crate) fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

fn require_env_name(value: &str, field: &str) -> Result<(), ConfigError> {
    if is_env_name(value) {
        Ok(())
    } else {
        Err(ConfigError::invalid(format!(
            "{field} must name an environment variable, not contain a literal value"
        )))
    }
}

fn strip_http_url(raw: &str, field: &str) -> Result<Option<String>, ConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(ConfigError::invalid(format!(
            "{field} must be http(s): {trimmed:?}"
        )));
    }
    Ok(Some(trimmed.trim_end_matches('/').to_string()))
}

/// Request class used by RAM / routing policy lists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestClass {
    Embed,
    Small,
    Medium,
    Large,
}

/// GPU selection strategy for Verda spots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStrategy {
    #[default]
    Cheapest,
    BestValue,
}

/// Static hardware capacity. `None` means the operator omitted the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capacity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
}

impl Capacity {
    /// Effective VRAM (0 when unset).
    pub fn vram_gb(&self) -> f64 {
        self.vram_gb.unwrap_or(0.0)
    }

    /// Effective RAM (0 when unset).
    pub fn ram_gb(&self) -> f64 {
        self.ram_gb.unwrap_or(0.0)
    }

    /// Effective GPU count (0 when unset).
    pub fn gpus(&self) -> u32 {
        self.gpus.unwrap_or(0)
    }

    /// Effective CPU cores (0 when unset).
    pub fn cpu_cores(&self) -> u32 {
        self.cpu_cores.unwrap_or(0)
    }

    /// Fields explicitly set by the operator.
    pub fn configured(&self) -> Vec<(&'static str, f64)> {
        let mut out = Vec::new();
        if let Some(v) = self.vram_gb {
            out.push(("vram_gb", v));
        }
        if let Some(v) = self.ram_gb {
            out.push(("ram_gb", v));
        }
        if let Some(v) = self.gpus {
            out.push(("gpus", f64::from(v)));
        }
        if let Some(v) = self.cpu_cores {
            out.push(("cpu_cores", f64::from(v)));
        }
        out
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [("vram_gb", self.vram_gb), ("ram_gb", self.ram_gb)] {
            if let Some(v) = value {
                if v < 0.0 {
                    return Err(ConfigError::invalid(format!(
                        "capacity values must be >= 0 ({name})"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A VRAM-gated tier of desired models.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelTier {
    pub models: Vec<String>,
    #[serde(default)]
    pub min_vram_gb: f64,
}

impl ModelTier {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.models.is_empty() {
            return Err(ConfigError::invalid(
                "desired_model_tiers.models must be non-empty",
            ));
        }
        if self.min_vram_gb < 0.0 {
            return Err(ConfigError::invalid("min_vram_gb must be >= 0"));
        }
        Ok(())
    }
}

/// One Ollama node in the fleet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub id: NodeId,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub capacity_url: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default, rename = "static")]
    pub static_capacity: Capacity,
    #[serde(default)]
    pub max_inflight: Option<u32>,
}

impl NodeConfig {
    /// Fields the operator set on `static:` / env capacity knobs.
    pub fn configured_capacity(&self) -> std::collections::BTreeMap<&'static str, f64> {
        self.static_capacity.configured().into_iter().collect()
    }

    pub(crate) fn normalize_and_validate(&mut self) -> Result<(), ConfigError> {
        if let Some(url) = self.url.take() {
            self.url = strip_http_url(&url, "node url")?;
        }
        if let Some(url) = self.capacity_url.take() {
            self.capacity_url = strip_http_url(&url, "capacity_url")?;
        }
        self.static_capacity.validate()?;
        if let Some(max) = self.max_inflight {
            if max < 1 {
                return Err(ConfigError::invalid("max_inflight must be >= 1 when set"));
            }
        }
        if self.url.is_none() {
            return Err(ConfigError::invalid(format!(
                "node {} needs a routing url",
                self.id
            )));
        }
        Ok(())
    }
}

/// Routing policy knobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub max_request_body_bytes: u64,
    pub small_max_b: f64,
    pub medium_max_b: f64,
    pub medium_min_vram_gb: f64,
    pub inflight_weight: f64,
    pub sticky_affinity: bool,
    pub must_have_labels: Vec<String>,
    pub avoid_labels: Vec<String>,
    pub prefer_warm_models: bool,
    pub vram_headroom: f64,
    pub vram_per_b: f64,
    pub vram_floor_gb: f64,
    pub medium_reserve_min_gb: f64,
    pub small_reserve_vram_gb: f64,
    pub embed_reserve_vram_gb: f64,
    pub ram_headroom: f64,
    pub ram_elevated_score_penalty: f64,
    pub ram_critical_score_penalty: f64,
    pub reject_on_ram_critical: bool,
    pub reject_on_ram_elevated_for_classes: Vec<RequestClass>,
    pub ram_sensitive_classes: Vec<RequestClass>,
    pub embed_reserve_ram_gb: f64,
    pub small_reserve_ram_gb: f64,
    pub gpu_system_ram_overhead_gb: f64,
    pub default_max_inflight: Option<u32>,
    pub retry_max_attempts: u32,
    pub retry_on_status: Vec<u16>,
    pub overload_wait_ms: u32,
    pub admission_wait_ms: u32,
    pub saturated_retry_after_seconds: u32,
    pub provision_retry_after_seconds: u32,
    pub auto_pull_on_miss: bool,
    pub pull_miss_retry_after_seconds: u32,
    pub auto_pull_wait_seconds: f64,
    pub model_warm_enabled: bool,
    pub model_warm_interval_seconds: f64,
    pub model_warm_min_free_vram_gb: f64,
    pub model_warm_cooldown_seconds: f64,
    pub model_warm_max_inflight_ratio: f64,
    pub health_recovery_ensure_enabled: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            max_request_body_bytes: 32 * 1024 * 1024,
            small_max_b: 4.0,
            medium_max_b: 9.0,
            medium_min_vram_gb: 8.0,
            inflight_weight: 1.0,
            sticky_affinity: false,
            must_have_labels: Vec::new(),
            avoid_labels: Vec::new(),
            prefer_warm_models: true,
            vram_headroom: 0.9,
            vram_per_b: 0.7,
            vram_floor_gb: 2.0,
            medium_reserve_min_gb: 8.0,
            small_reserve_vram_gb: 0.0,
            embed_reserve_vram_gb: 0.0,
            ram_headroom: 0.85,
            ram_elevated_score_penalty: 2.0,
            ram_critical_score_penalty: 8.0,
            reject_on_ram_critical: true,
            reject_on_ram_elevated_for_classes: vec![RequestClass::Medium, RequestClass::Large],
            ram_sensitive_classes: vec![
                RequestClass::Embed,
                RequestClass::Small,
                RequestClass::Medium,
                RequestClass::Large,
            ],
            embed_reserve_ram_gb: 4.0,
            small_reserve_ram_gb: 0.0,
            gpu_system_ram_overhead_gb: 1.0,
            default_max_inflight: None,
            retry_max_attempts: 3,
            retry_on_status: vec![429, 503],
            overload_wait_ms: 0,
            admission_wait_ms: 0,
            saturated_retry_after_seconds: 30,
            provision_retry_after_seconds: 30,
            auto_pull_on_miss: false,
            pull_miss_retry_after_seconds: 10,
            auto_pull_wait_seconds: 0.0,
            model_warm_enabled: true,
            model_warm_interval_seconds: 60.0,
            model_warm_min_free_vram_gb: 4.0,
            model_warm_cooldown_seconds: 30.0,
            model_warm_max_inflight_ratio: 0.5,
            health_recovery_ensure_enabled: true,
        }
    }
}

impl PolicyConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("small_max_b", self.small_max_b),
            ("medium_max_b", self.medium_max_b),
            ("medium_min_vram_gb", self.medium_min_vram_gb),
            ("inflight_weight", self.inflight_weight),
            ("vram_headroom", self.vram_headroom),
            ("vram_per_b", self.vram_per_b),
            ("vram_floor_gb", self.vram_floor_gb),
            ("medium_reserve_min_gb", self.medium_reserve_min_gb),
        ] {
            if value <= 0.0 {
                return Err(ConfigError::invalid(format!(
                    "policy thresholds must be > 0 ({name})"
                )));
            }
        }
        if self.max_request_body_bytes < 1 || self.max_request_body_bytes > 1024 * 1024 * 1024 {
            return Err(ConfigError::invalid(
                "max_request_body_bytes must be between 1 byte and 1 GiB",
            ));
        }
        if let Some(max) = self.default_max_inflight {
            if max < 1 {
                return Err(ConfigError::invalid(
                    "default_max_inflight must be >= 1 when set",
                ));
            }
        }
        if self.retry_max_attempts < 1 {
            return Err(ConfigError::invalid("retry_max_attempts must be >= 1"));
        }
        for status in &self.retry_on_status {
            if !(400..=599).contains(status) {
                return Err(ConfigError::invalid(format!(
                    "retry_on_status entries must be 4xx/5xx: {status}"
                )));
            }
        }
        if self.small_reserve_vram_gb < 0.0 || self.embed_reserve_vram_gb < 0.0 {
            return Err(ConfigError::invalid("flat reservation knobs must be >= 0"));
        }
        for (name, value) in [
            (
                "ram_elevated_score_penalty",
                self.ram_elevated_score_penalty,
            ),
            (
                "ram_critical_score_penalty",
                self.ram_critical_score_penalty,
            ),
            (
                "gpu_system_ram_overhead_gb",
                self.gpu_system_ram_overhead_gb,
            ),
            ("embed_reserve_ram_gb", self.embed_reserve_ram_gb),
            ("small_reserve_ram_gb", self.small_reserve_ram_gb),
        ] {
            if value < 0.0 {
                return Err(ConfigError::invalid(format!(
                    "RAM policy values must be >= 0 ({name})"
                )));
            }
        }
        if self.ram_headroom <= 0.0 || self.ram_headroom > 1.0 {
            return Err(ConfigError::invalid("ram_headroom must be > 0 and <= 1"));
        }
        if !(1..=900).contains(&self.saturated_retry_after_seconds) {
            return Err(ConfigError::invalid(
                "saturated_retry_after_seconds must be between 1 and 900",
            ));
        }
        if !(1..=900).contains(&self.provision_retry_after_seconds) {
            return Err(ConfigError::invalid(
                "provision_retry_after_seconds must be between 1 and 900",
            ));
        }
        if !(1..=900).contains(&self.pull_miss_retry_after_seconds) {
            return Err(ConfigError::invalid(
                "pull_miss_retry_after_seconds must be between 1 and 900",
            ));
        }
        if !(0.0..=120.0).contains(&self.auto_pull_wait_seconds) {
            return Err(ConfigError::invalid(
                "auto_pull_wait_seconds must be between 0 and 120",
            ));
        }
        if self.model_warm_interval_seconds <= 0.0 || self.model_warm_cooldown_seconds <= 0.0 {
            return Err(ConfigError::invalid(
                "model warm interval/cooldown must be > 0",
            ));
        }
        if self.model_warm_min_free_vram_gb < 0.0 {
            return Err(ConfigError::invalid(
                "model_warm_min_free_vram_gb must be >= 0",
            ));
        }
        if self.model_warm_max_inflight_ratio <= 0.0 || self.model_warm_max_inflight_ratio > 1.0 {
            return Err(ConfigError::invalid(
                "model_warm_max_inflight_ratio must be > 0 and <= 1",
            ));
        }
        Ok(())
    }
}

/// Health probe + circuit breaker tuning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    pub interval_seconds: f64,
    pub probe_timeout_seconds: f64,
    pub fail_streak_threshold: u32,
    pub success_threshold: u32,
    pub backoff_max_seconds: f64,
    pub request_fail_credit: u32,
    pub overload_fail_credit: u32,
    pub ps_probe_enabled: bool,
    pub capacity_probe_enabled: bool,
    pub capacity_probe_port: u16,
    pub capacity_probe_path: String,
    pub capacity_probe_timeout_seconds: f64,
    pub capacity_probe_token: Option<String>,
    pub capacity_probe_every_n_probes: u32,
    pub pressure_probe_path: Option<String>,
    pub probe_jitter_ratio: f64,
    pub max_concurrent_probes: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            interval_seconds: 5.0,
            probe_timeout_seconds: 3.0,
            fail_streak_threshold: 3,
            success_threshold: 1,
            backoff_max_seconds: 60.0,
            request_fail_credit: 1,
            overload_fail_credit: 1,
            ps_probe_enabled: true,
            capacity_probe_enabled: true,
            capacity_probe_port: 11436,
            capacity_probe_path: "/v1/capacity".to_string(),
            capacity_probe_timeout_seconds: 2.0,
            capacity_probe_token: None,
            capacity_probe_every_n_probes: 1,
            pressure_probe_path: None,
            probe_jitter_ratio: 0.2,
            max_concurrent_probes: 8,
        }
    }
}

impl HealthConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("interval_seconds", self.interval_seconds),
            ("probe_timeout_seconds", self.probe_timeout_seconds),
            ("backoff_max_seconds", self.backoff_max_seconds),
            (
                "capacity_probe_timeout_seconds",
                self.capacity_probe_timeout_seconds,
            ),
        ] {
            if value <= 0.0 {
                return Err(ConfigError::invalid(format!(
                    "health timings must be > 0 ({name})"
                )));
            }
        }
        if self.fail_streak_threshold == 0
            || self.success_threshold == 0
            || self.request_fail_credit == 0
        {
            return Err(ConfigError::invalid("health thresholds must be > 0"));
        }
        if self.capacity_probe_port == 0 {
            return Err(ConfigError::invalid(
                "capacity_probe_port must be between 1 and 65535",
            ));
        }
        if self.capacity_probe_every_n_probes < 1 {
            return Err(ConfigError::invalid(
                "capacity_probe_every_n_probes must be >= 1",
            ));
        }
        if !self.capacity_probe_path.starts_with('/') {
            return Err(ConfigError::invalid("probe paths must start with '/'"));
        }
        if let Some(path) = &self.pressure_probe_path {
            if !path.starts_with('/') {
                return Err(ConfigError::invalid("probe paths must start with '/'"));
            }
        }
        if self.overload_fail_credit > self.request_fail_credit {
            return Err(ConfigError::invalid(
                "overload_fail_credit must be <= request_fail_credit",
            ));
        }
        if self.probe_jitter_ratio < 0.0 || self.probe_jitter_ratio > 1.0 {
            return Err(ConfigError::invalid(
                "probe_jitter_ratio must be between 0 and 1",
            ));
        }
        if self.max_concurrent_probes < 1 {
            return Err(ConfigError::invalid("max_concurrent_probes must be >= 1"));
        }
        Ok(())
    }
}

/// Upstream request timeouts per request class (seconds).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeoutsConfig {
    pub connect_seconds: f64,
    pub default_seconds: f64,
    pub embed_seconds: f64,
    pub generate_seconds: f64,
    pub pull_seconds: f64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            connect_seconds: 5.0,
            default_seconds: 30.0,
            embed_seconds: 300.0,
            generate_seconds: 600.0,
            pull_seconds: 3600.0,
        }
    }
}

impl TimeoutsConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in [
            ("connect_seconds", self.connect_seconds),
            ("default_seconds", self.default_seconds),
            ("embed_seconds", self.embed_seconds),
            ("generate_seconds", self.generate_seconds),
            ("pull_seconds", self.pull_seconds),
        ] {
            if value <= 0.0 {
                return Err(ConfigError::invalid(format!(
                    "timeouts must be > 0 ({name})"
                )));
            }
        }
        Ok(())
    }
}

/// reqwest / hyper connection-pool limits (Python httpx 256/32).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpstreamPoolConfig {
    pub max_connections: u32,
    pub max_keepalive_connections: u32,
}

impl Default for UpstreamPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 256,
            max_keepalive_connections: 32,
        }
    }
}

impl UpstreamPoolConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.max_connections < 1 {
            return Err(ConfigError::invalid(
                "upstream.max_connections must be >= 1",
            ));
        }
        if self.max_keepalive_connections < 1 {
            return Err(ConfigError::invalid(
                "upstream.max_keepalive_connections must be >= 1",
            ));
        }
        Ok(())
    }
}

fn default_zrok_bin() -> String {
    "zrok".to_string()
}

fn default_public_share_suffixes() -> Vec<String> {
    vec![".zrok.io".to_string()]
}

fn default_enable_token_env() -> String {
    "ZROK_ENABLE_TOKEN".to_string()
}

fn default_access_bind() -> String {
    "127.0.0.1".to_string()
}

/// `host:port` for `TcpListener` / `zrok --bindAddress` (bracket IPv6).
pub fn socket_addr_for_bind(bind: &str, port: u16) -> String {
    if bind.contains(':') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    }
}

/// `http://` URL for an access frontend on `bind`.
pub fn http_url_for_bind(bind: &str, port: u16) -> String {
    if bind.contains(':') {
        format!("http://[{bind}]:{port}")
    } else {
        format!("http://{bind}:{port}")
    }
}

fn validate_loopback_bind(bind: &str, field: &str) -> Result<(), ConfigError> {
    let trimmed = bind.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::invalid(format!(
            "{field} must be a loopback address"
        )));
    }
    let ip: IpAddr = trimmed.parse().map_err(|_| {
        ConfigError::invalid(format!("{field} must be a loopback IP, not {trimmed:?}"))
    })?;
    if !ip.is_loopback() {
        return Err(ConfigError::invalid(format!(
            "{field} must be a loopback address, not {trimmed}"
        )));
    }
    Ok(())
}

/// Router-side zrok access + public-share hostname denylist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TunnelConfig {
    #[serde(default = "default_zrok_bin")]
    pub zrok_bin: String,
    /// Extra public-share suffixes. `.zrok.io` is always blocked.
    #[serde(default = "default_public_share_suffixes")]
    pub public_share_suffixes: Vec<String>,
    /// Self-hosted zrok controller API (`ZROK_API_ENDPOINT`). Empty uses the
    /// process env / `zrok` config file. Never `zrok.io` for this product.
    #[serde(default)]
    pub api_endpoint: String,
    /// Env **name** holding the enable token for `zrok enable`. Never a literal.
    #[serde(default = "default_enable_token_env")]
    pub enable_token_env: String,
    /// Loopback host for `zrok access private --bindAddress` (default `127.0.0.1`).
    #[serde(default = "default_access_bind")]
    pub access_bind: String,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            zrok_bin: default_zrok_bin(),
            public_share_suffixes: default_public_share_suffixes(),
            api_endpoint: String::new(),
            enable_token_env: default_enable_token_env(),
            access_bind: default_access_bind(),
        }
    }
}

impl TunnelConfig {
    /// Trimmed controller API URL, if configured.
    pub fn api_endpoint(&self) -> Option<&str> {
        let trimmed = self.api_endpoint.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Loopback HTTP URL for an access frontend port.
    pub fn loopback_http_url(&self, port: u16) -> String {
        http_url_for_bind(self.access_bind.trim(), port)
    }

    /// Socket address for an access frontend port.
    pub fn access_socket_addr(&self, port: u16) -> String {
        socket_addr_for_bind(self.access_bind.trim(), port)
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.zrok_bin.trim().is_empty() {
            return Err(ConfigError::invalid("tunnel.zrok_bin must be non-empty"));
        }
        for suffix in &self.public_share_suffixes {
            if suffix.trim().is_empty() {
                return Err(ConfigError::invalid(
                    "tunnel.public_share_suffixes must not contain empty values",
                ));
            }
        }
        strip_http_url(&self.api_endpoint, "tunnel.api_endpoint")?;
        require_env_name(&self.enable_token_env, "tunnel.enable_token_env")?;
        validate_loopback_bind(&self.access_bind, "tunnel.access_bind")?;
        Ok(())
    }
}

/// Verda Cloud spot GPU provisioning (opt-in; disabled by default).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerdaConfig {
    pub enabled: bool,
    pub base_url: String,
    pub client_id_env: String,
    pub client_secret_env: String,
    pub auto_scale: bool,
    pub auto_scale_min_instances: u32,
    pub auto_scale_max_instances: u32,
    pub demand_scale_price_per_hour: Option<f64>,
    pub idle_scale_down_enabled: bool,
    pub idle_timeout_seconds: f64,
    pub idle_grace_after_create_seconds: f64,
    pub orphan_reclaim_enabled: bool,
    pub orphan_reclaim_grace_seconds: f64,
    pub ensure_on_startup: bool,
    pub destroy_on_shutdown: bool,
    pub router_id_env: String,
    pub ssh_key_id: Option<String>,
    pub ssh_key_name: String,
    pub ssh_public_key_file: Option<String>,
    pub ssh_private_key_file: Option<String>,
    /// Pre-created Verda startup script id. When set, create skips list/create.
    pub startup_script_id: Option<String>,
    /// Reuse-by-name catalog script (installer body; secrets injected at create).
    pub startup_script_name: String,
    /// Optional override for the agent `.deb` / tarball (wins over GitHub release).
    pub agent_package_url: Option<String>,
    /// GitHub `owner/repo` used to build default release URLs.
    pub agent_github_repo: String,
    /// Agent package version (`v{version}` tag). Empty → crate version.
    pub agent_version: Option<String>,
    /// Router URL the spot can reach for `POST /router/v1/nodes/enroll`.
    pub enroll_url: Option<String>,
    /// Env **name** holding the zrok enable token (setup only). Never a literal.
    pub zrok_enable_token_env: String,
    /// Env **name** holding `OLLAMA_ROUTER_ADMIN_TOKEN` (or equivalent).
    pub enroll_token_env: String,
    pub preferred_image_globs: Vec<String>,
    pub min_vram_gb: f64,
    pub max_vram_gb: Option<f64>,
    pub min_gpus: u32,
    pub max_gpus: Option<u32>,
    pub allowed_instance_types: Vec<String>,
    pub denied_instance_types: Vec<String>,
    pub allowed_locations: Vec<String>,
    pub max_spot_price_per_hour: Option<f64>,
    pub selection_strategy: SelectionStrategy,
    pub os_volume_gb: u32,
    pub on_spot_discontinue: String,
    pub poll_interval_seconds: f64,
    pub create_timeout_seconds: f64,
    pub destroy_timeout_seconds: f64,
    pub create_retries: u32,
    pub create_backoff_base_seconds: f64,
}

impl Default for VerdaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.verda.com".to_string(),
            client_id_env: "VERDA_CLIENT_ID".to_string(),
            client_secret_env: "VERDA_CLIENT_SECRET".to_string(),
            auto_scale: true,
            auto_scale_min_instances: 0,
            auto_scale_max_instances: 2,
            demand_scale_price_per_hour: None,
            idle_scale_down_enabled: true,
            idle_timeout_seconds: 900.0,
            idle_grace_after_create_seconds: 300.0,
            orphan_reclaim_enabled: true,
            orphan_reclaim_grace_seconds: 1800.0,
            ensure_on_startup: false,
            destroy_on_shutdown: true,
            router_id_env: "OLLAMA_ROUTER_ID".to_string(),
            ssh_key_id: None,
            ssh_key_name: "ollama-router".to_string(),
            ssh_public_key_file: None,
            ssh_private_key_file: None,
            startup_script_id: None,
            startup_script_name: "ollama-router-agent-init".to_string(),
            agent_package_url: None,
            agent_github_repo: "miguelenes/ollama-router".to_string(),
            agent_version: None,
            enroll_url: None,
            zrok_enable_token_env: "ZROK_ENABLE_TOKEN".to_string(),
            enroll_token_env: "OLLAMA_ROUTER_ADMIN_TOKEN".to_string(),
            preferred_image_globs: vec![
                "*ubuntu-24*cuda*docker*".to_string(),
                "*ubuntu-24*docker*".to_string(),
                "ubuntu-24.04".to_string(),
            ],
            min_vram_gb: 8.0,
            max_vram_gb: Some(80.0),
            min_gpus: 1,
            max_gpus: None,
            allowed_instance_types: Vec::new(),
            denied_instance_types: Vec::new(),
            allowed_locations: Vec::new(),
            max_spot_price_per_hour: None,
            selection_strategy: SelectionStrategy::Cheapest,
            os_volume_gb: 100,
            on_spot_discontinue: "delete_permanently".to_string(),
            poll_interval_seconds: 10.0,
            create_timeout_seconds: 900.0,
            destroy_timeout_seconds: 120.0,
            create_retries: 2,
            create_backoff_base_seconds: 2.0,
        }
    }
}

impl VerdaConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        let base = self.base_url.trim().trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(ConfigError::invalid(format!(
                "verda.base_url must be http(s): {base:?}"
            )));
        }
        require_env_name(&self.client_id_env, "verda.client_id_env")?;
        require_env_name(&self.client_secret_env, "verda.client_secret_env")?;
        require_env_name(&self.router_id_env, "verda.router_id_env")?;
        require_env_name(&self.zrok_enable_token_env, "verda.zrok_enable_token_env")?;
        require_env_name(&self.enroll_token_env, "verda.enroll_token_env")?;
        if self.startup_script_name.trim().is_empty() {
            return Err(ConfigError::invalid(
                "verda.startup_script_name must be non-empty",
            ));
        }
        if let Some(id) = self.startup_script_id.as_deref() {
            if id.trim().is_empty() {
                return Err(ConfigError::invalid(
                    "verda.startup_script_id must be non-empty when set",
                ));
            }
        }
        if let Some(raw) = self.enroll_url.as_deref() {
            let _ = strip_http_url(raw, "verda.enroll_url")?;
        }
        if let Some(raw) = self.agent_package_url.as_deref() {
            let _ = strip_http_url(raw, "verda.agent_package_url")?;
        }
        if self.agent_github_repo.trim().is_empty()
            || !self.agent_github_repo.contains('/')
            || self.agent_github_repo.contains("://")
        {
            return Err(ConfigError::invalid(
                "verda.agent_github_repo must be owner/repo",
            ));
        }
        if self.min_vram_gb < 0.0 {
            return Err(ConfigError::invalid("verda VRAM bounds must be >= 0"));
        }
        if let Some(max) = self.max_vram_gb {
            if max < 0.0 {
                return Err(ConfigError::invalid("verda VRAM bounds must be >= 0"));
            }
            if max < self.min_vram_gb {
                return Err(ConfigError::invalid(
                    "verda.max_vram_gb must be >= min_vram_gb",
                ));
            }
        }
        if let Some(price) = self.demand_scale_price_per_hour {
            if price < 0.0 {
                return Err(ConfigError::invalid("verda VRAM bounds must be >= 0"));
            }
        }
        if self.min_gpus < 1 {
            return Err(ConfigError::invalid("verda.min_gpus must be >= 1"));
        }
        if self.os_volume_gb < 10 {
            return Err(ConfigError::invalid("verda.os_volume_gb must be >= 10"));
        }
        for (name, value) in [
            ("poll_interval_seconds", self.poll_interval_seconds),
            ("create_timeout_seconds", self.create_timeout_seconds),
            ("destroy_timeout_seconds", self.destroy_timeout_seconds),
        ] {
            if value <= 0.0 {
                return Err(ConfigError::invalid(format!(
                    "verda timings must be > 0 ({name})"
                )));
            }
        }
        if self.idle_timeout_seconds < 0.0
            || self.idle_grace_after_create_seconds < 0.0
            || self.create_backoff_base_seconds < 0.0
            || self.orphan_reclaim_grace_seconds < 0.0
        {
            return Err(ConfigError::invalid("verda idle timings must be >= 0"));
        }
        if self.orphan_reclaim_enabled
            && self.orphan_reclaim_grace_seconds < self.create_timeout_seconds
        {
            return Err(ConfigError::invalid(
                "verda.orphan_reclaim_grace_seconds must be >= create_timeout_seconds",
            ));
        }
        if self.auto_scale_max_instances > 0
            && self.auto_scale_min_instances > self.auto_scale_max_instances
        {
            return Err(ConfigError::invalid("verda auto_scale min must be <= max"));
        }
        Ok(())
    }

    /// Credential from the env var named by `client_id_env`.
    pub fn client_id(&self, env: &impl super::env_source::EnvSource) -> Option<String> {
        env.var(&self.client_id_env)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Credential from the env var named by `client_secret_env`. Never log this.
    pub fn client_secret(&self, env: &impl super::env_source::EnvSource) -> Option<String> {
        env.var(&self.client_secret_env)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Resolve the stable router identity: env var named by `router_id_env`, else hostname.
    pub fn router_id(
        &self,
        env: &impl super::env_source::EnvSource,
    ) -> crate::fleet::ids::RouterId {
        if let Some(value) = env.var(&self.router_id_env) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                if let Ok(id) = crate::fleet::ids::RouterId::parse(trimmed) {
                    return id;
                }
            }
        }
        crate::fleet::ids::RouterId::parse(system_hostname())
            .unwrap_or_else(|_| crate::fleet::ids::RouterId::fallback())
    }
}

fn system_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ollama-router".to_string())
}

/// YAML tunables only — no `nodes` field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YamlTunables {
    pub policy: PolicyConfig,
    pub health: HealthConfig,
    pub timeouts: TimeoutsConfig,
    pub desired_model_tiers: Vec<ModelTier>,
    pub ready_requires_embedding_model: bool,
    pub bootstrap_desired_models: bool,
    pub bootstrap_probe_wait_seconds: f64,
    pub bootstrap_require_ram_headroom: bool,
    pub bootstrap_require_capacity: bool,
    pub debug_headers: bool,
    pub max_pulls_per_node: u32,
    pub job_store_path: Option<String>,
    pub jobs_max_retained: u32,
    pub jobs_retention_seconds: u32,
    pub ensure_wait_max_seconds: f64,
    pub listen_host: String,
    pub listen_port: u16,
    pub verda: VerdaConfig,
    pub upstream: UpstreamPoolConfig,
    pub tunnel: TunnelConfig,
}

impl Default for YamlTunables {
    fn default() -> Self {
        Self {
            policy: PolicyConfig::default(),
            health: HealthConfig::default(),
            timeouts: TimeoutsConfig::default(),
            desired_model_tiers: Vec::new(),
            ready_requires_embedding_model: false,
            bootstrap_desired_models: false,
            bootstrap_probe_wait_seconds: 10.0,
            bootstrap_require_ram_headroom: true,
            bootstrap_require_capacity: true,
            debug_headers: true,
            max_pulls_per_node: 1,
            job_store_path: None,
            jobs_max_retained: 256,
            jobs_retention_seconds: 3600,
            ensure_wait_max_seconds: 300.0,
            listen_host: "0.0.0.0".to_string(),
            listen_port: 11434,
            verda: VerdaConfig::default(),
            upstream: UpstreamPoolConfig::default(),
            tunnel: TunnelConfig::default(),
        }
    }
}

impl YamlTunables {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.policy.validate()?;
        self.health.validate()?;
        self.timeouts.validate()?;
        self.verda.validate()?;
        self.upstream.validate()?;
        self.tunnel.validate()?;
        for tier in &self.desired_model_tiers {
            tier.validate()?;
        }
        if self.max_pulls_per_node == 0 || self.jobs_max_retained == 0 {
            return Err(ConfigError::invalid("value must be > 0"));
        }
        if self.jobs_retention_seconds < 60 {
            return Err(ConfigError::invalid(
                "jobs_retention_seconds must be >= 60 to keep in-flight jobs inspectable",
            ));
        }
        if let Some(path) = &self.job_store_path {
            if path.trim().is_empty() {
                return Err(ConfigError::invalid(
                    "job_store_path must be non-empty when configured",
                ));
            }
        }
        if self.ensure_wait_max_seconds <= 0.0 {
            return Err(ConfigError::invalid("ensure_wait_max_seconds must be > 0"));
        }
        if self.listen_port == 0 {
            return Err(ConfigError::invalid(
                "listen_port must be between 1 and 65535",
            ));
        }
        if self.bootstrap_probe_wait_seconds < 0.0 {
            return Err(ConfigError::invalid(
                "bootstrap_probe_wait_seconds must be >= 0",
            ));
        }
        Ok(())
    }
}

/// Top-level router configuration (tunables + fleet.yaml nodes).
#[derive(Clone, Debug, PartialEq)]
pub struct RouterConfig {
    pub nodes: Vec<NodeConfig>,
    pub policy: PolicyConfig,
    pub health: HealthConfig,
    pub timeouts: TimeoutsConfig,
    pub desired_model_tiers: Vec<ModelTier>,
    pub ready_requires_embedding_model: bool,
    pub bootstrap_desired_models: bool,
    pub bootstrap_probe_wait_seconds: f64,
    pub bootstrap_require_ram_headroom: bool,
    pub bootstrap_require_capacity: bool,
    pub debug_headers: bool,
    pub max_pulls_per_node: u32,
    pub job_store_path: Option<String>,
    pub jobs_max_retained: u32,
    pub jobs_retention_seconds: u32,
    pub ensure_wait_max_seconds: f64,
    pub listen_host: String,
    pub listen_port: u16,
    pub verda: VerdaConfig,
    pub upstream: UpstreamPoolConfig,
    pub fleet_path: PathBuf,
    pub fleet_missing_is_error: bool,
    pub state_path: PathBuf,
    pub tunnel: TunnelConfig,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self::from_tunables(YamlTunables::default(), Vec::new())
    }
}

impl RouterConfig {
    pub(crate) fn from_tunables(tunables: YamlTunables, nodes: Vec<NodeConfig>) -> Self {
        Self {
            nodes,
            policy: tunables.policy,
            health: tunables.health,
            timeouts: tunables.timeouts,
            desired_model_tiers: tunables.desired_model_tiers,
            ready_requires_embedding_model: tunables.ready_requires_embedding_model,
            bootstrap_desired_models: tunables.bootstrap_desired_models,
            bootstrap_probe_wait_seconds: tunables.bootstrap_probe_wait_seconds,
            bootstrap_require_ram_headroom: tunables.bootstrap_require_ram_headroom,
            bootstrap_require_capacity: tunables.bootstrap_require_capacity,
            debug_headers: tunables.debug_headers,
            max_pulls_per_node: tunables.max_pulls_per_node,
            job_store_path: tunables.job_store_path,
            jobs_max_retained: tunables.jobs_max_retained,
            jobs_retention_seconds: tunables.jobs_retention_seconds,
            ensure_wait_max_seconds: tunables.ensure_wait_max_seconds,
            listen_host: tunables.listen_host,
            listen_port: tunables.listen_port,
            verda: tunables.verda,
            upstream: tunables.upstream,
            fleet_path: PathBuf::from("/etc/ollama-router/fleet.yaml"),
            fleet_missing_is_error: false,
            state_path: PathBuf::from("/var/lib/ollama-router/fleet-state.json"),
            tunnel: tunables.tunnel,
        }
    }

    pub(crate) fn validate_nodes(&self) -> Result<(), ConfigError> {
        reject_duplicate_node_ids(&self.nodes)
    }

    /// Configured VRAM-gated desired model tiers (no legacy flat list).
    pub fn effective_model_tiers(&self) -> Vec<ModelTier> {
        self.desired_model_tiers.clone()
    }

    /// Union of tier models whose `min_vram_gb` the node meets.
    ///
    /// CPU (`vram_gb = 0`) only sees tiers with `min_vram_gb <= 0`.
    pub fn tier_models_for_vram(&self, vram_gb: f64) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for tier in self.effective_model_tiers() {
            if vram_gb < tier.min_vram_gb {
                continue;
            }
            for model in tier.models {
                if seen.insert(model.clone()) {
                    out.push(model);
                }
            }
        }
        out
    }
}

pub(crate) fn reject_duplicate_node_ids(nodes: &[NodeConfig]) -> Result<(), ConfigError> {
    let mut seen = std::collections::HashSet::new();
    for node in nodes {
        if !seen.insert(node.id.as_str()) {
            return Err(ConfigError::invalid("node ids must be unique"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verda_vram_defaults_are_inclusive_8_to_80() {
        let verda = VerdaConfig::default();
        assert_eq!(verda.min_vram_gb, 8.0);
        assert_eq!(verda.max_vram_gb, Some(80.0));
    }

    #[test]
    fn verda_null_max_vram_removes_ceiling() {
        let verda: VerdaConfig =
            serde_yaml::from_str("min_vram_gb: 8\nmax_vram_gb: null\n").unwrap();
        verda.validate().unwrap();
        assert_eq!(verda.min_vram_gb, 8.0);
        assert_eq!(verda.max_vram_gb, None);
    }

    #[test]
    fn verda_rejects_max_below_min_and_negatives() {
        let max_below_min = VerdaConfig {
            min_vram_gb: 48.0,
            max_vram_gb: Some(24.0),
            ..VerdaConfig::default()
        };
        assert!(max_below_min.validate().is_err());

        let negative_max = VerdaConfig {
            min_vram_gb: 8.0,
            max_vram_gb: Some(-1.0),
            ..VerdaConfig::default()
        };
        assert!(negative_max.validate().is_err());
    }

    #[test]
    fn env_name_rejects_literals() {
        assert!(is_env_name("VERDA_CLIENT_SECRET"));
        assert!(!is_env_name("literal-secret-value"));
        assert!(!is_env_name("tskey-auth-literal"));
    }

    #[test]
    fn orphan_reclaim_grace_must_cover_create_timeout_when_enabled() {
        let mut verda = VerdaConfig::default();
        verda.validate().unwrap();
        verda.orphan_reclaim_grace_seconds = verda.create_timeout_seconds - 1.0;
        assert!(verda.validate().is_err());
        verda.orphan_reclaim_enabled = false;
        verda.validate().unwrap();
    }

    #[test]
    fn verda_startup_script_knobs_validate() {
        let mut verda = VerdaConfig::default();
        verda.validate().unwrap();
        verda.enroll_url = Some("not-a-url".into());
        assert!(verda.validate().is_err());
        verda.enroll_url = Some("https://router.example:11435".into());
        verda.zrok_enable_token_env = "literal-token".into();
        assert!(verda.validate().is_err());
    }

    #[test]
    fn tunnel_defaults_are_loopback_and_env_name() {
        let tunnel = TunnelConfig::default();
        tunnel.validate().unwrap();
        assert!(tunnel.api_endpoint().is_none());
        assert_eq!(tunnel.enable_token_env, "ZROK_ENABLE_TOKEN");
        assert_eq!(tunnel.access_bind, "127.0.0.1");
        assert_eq!(tunnel.loopback_http_url(41990), "http://127.0.0.1:41990");
        assert_eq!(tunnel.access_socket_addr(41990), "127.0.0.1:41990");
    }

    #[test]
    fn tunnel_rejects_non_loopback_bind_and_literal_token() {
        let bad_bind = TunnelConfig {
            access_bind: "0.0.0.0".into(),
            ..TunnelConfig::default()
        };
        assert!(bad_bind.validate().is_err());
        let bad_env = TunnelConfig {
            enable_token_env: "not-an-env".into(),
            ..TunnelConfig::default()
        };
        assert!(bad_env.validate().is_err());
        let bad_url = TunnelConfig {
            api_endpoint: "zrok.example".into(),
            ..TunnelConfig::default()
        };
        assert!(bad_url.validate().is_err());
        let ok = TunnelConfig {
            api_endpoint: "http://127.0.0.1:18080".into(),
            ..TunnelConfig::default()
        };
        ok.validate().unwrap();
    }

    #[test]
    fn tunnel_unknown_field_is_denied() {
        let err = serde_yaml::from_str::<TunnelConfig>("zrok_bin: zrok\nfoo: 1\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn ipv6_loopback_bind_urls_use_brackets() {
        assert_eq!(http_url_for_bind("::1", 9), "http://[::1]:9");
        assert_eq!(socket_addr_for_bind("::1", 9), "[::1]:9");
    }
}
