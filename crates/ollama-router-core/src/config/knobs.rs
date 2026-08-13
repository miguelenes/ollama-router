//! Table-driven `OLLAMA_ROUTER_*` / `VERDA_*` env knobs.

use serde_yaml::{Mapping, Value};

use super::env_source::EnvSource;
use super::error::ConfigError;

const TRUE_VALUES: &[&str] = &["1", "true", "yes", "on"];
const FALSE_VALUES: &[&str] = &["0", "false", "no", "off"];

#[derive(Clone, Copy)]
enum KnobKind {
    Bool,
    Str,
    Int,
    Float,
    Csv,
}

struct Knob {
    env: &'static str,
    path: &'static [&'static str],
    kind: KnobKind,
}

const KNOBS: &[Knob] = &[
    Knob {
        env: "OLLAMA_ROUTER_DEBUG_HEADERS",
        path: &["debug_headers"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "OLLAMA_ROUTER_MODEL_WARM_ENABLED",
        path: &["policy", "model_warm_enabled"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "OLLAMA_ROUTER_MODEL_WARM_INTERVAL_SECONDS",
        path: &["policy", "model_warm_interval_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "OLLAMA_ROUTER_MODEL_WARM_MIN_FREE_VRAM_GB",
        path: &["policy", "model_warm_min_free_vram_gb"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "OLLAMA_ROUTER_MODEL_WARM_COOLDOWN_SECONDS",
        path: &["policy", "model_warm_cooldown_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "OLLAMA_ROUTER_MODEL_WARM_MAX_INFLIGHT_RATIO",
        path: &["policy", "model_warm_max_inflight_ratio"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "OLLAMA_CAPACITY_TOKEN",
        path: &["health", "capacity_probe_token"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_ENABLED",
        path: &["verda", "enabled"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "VERDA_AUTO_SCALE",
        path: &["verda", "auto_scale"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "VERDA_AUTO_SCALE_MIN_INSTANCES",
        path: &["verda", "auto_scale_min_instances"],
        kind: KnobKind::Int,
    },
    Knob {
        env: "VERDA_AUTO_SCALE_MAX_INSTANCES",
        path: &["verda", "auto_scale_max_instances"],
        kind: KnobKind::Int,
    },
    Knob {
        env: "VERDA_DEMAND_SCALE_PRICE_PER_HOUR",
        path: &["verda", "demand_scale_price_per_hour"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_IDLE_SCALE_DOWN_ENABLED",
        path: &["verda", "idle_scale_down_enabled"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "VERDA_IDLE_TIMEOUT_SECONDS",
        path: &["verda", "idle_timeout_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_IDLE_GRACE_AFTER_CREATE_SECONDS",
        path: &["verda", "idle_grace_after_create_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_ENSURE_ON_STARTUP",
        path: &["verda", "ensure_on_startup"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "VERDA_DESTROY_ON_SHUTDOWN",
        path: &["verda", "destroy_on_shutdown"],
        kind: KnobKind::Bool,
    },
    Knob {
        env: "VERDA_BASE_URL",
        path: &["verda", "base_url"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_SSH_KEY_ID",
        path: &["verda", "ssh_key_id"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_SSH_KEY_NAME",
        path: &["verda", "ssh_key_name"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_SSH_PUBLIC_KEY_FILE",
        path: &["verda", "ssh_public_key_file"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_SSH_PRIVATE_KEY_FILE",
        path: &["verda", "ssh_private_key_file"],
        kind: KnobKind::Str,
    },
    Knob {
        env: "VERDA_MIN_VRAM_GB",
        path: &["verda", "min_vram_gb"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_MAX_VRAM_GB",
        path: &["verda", "max_vram_gb"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_MIN_GPUS",
        path: &["verda", "min_gpus"],
        kind: KnobKind::Int,
    },
    Knob {
        env: "VERDA_MAX_GPUS",
        path: &["verda", "max_gpus"],
        kind: KnobKind::Int,
    },
    Knob {
        env: "VERDA_MAX_SPOT_PRICE_PER_HOUR",
        path: &["verda", "max_spot_price_per_hour"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_ALLOWED_LOCATIONS",
        path: &["verda", "allowed_locations"],
        kind: KnobKind::Csv,
    },
    Knob {
        env: "VERDA_ALLOWED_INSTANCE_TYPES",
        path: &["verda", "allowed_instance_types"],
        kind: KnobKind::Csv,
    },
    Knob {
        env: "VERDA_DENIED_INSTANCE_TYPES",
        path: &["verda", "denied_instance_types"],
        kind: KnobKind::Csv,
    },
    Knob {
        env: "VERDA_OS_VOLUME_GB",
        path: &["verda", "os_volume_gb"],
        kind: KnobKind::Int,
    },
    Knob {
        env: "VERDA_POLL_INTERVAL_SECONDS",
        path: &["verda", "poll_interval_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_CREATE_TIMEOUT_SECONDS",
        path: &["verda", "create_timeout_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_DESTROY_TIMEOUT_SECONDS",
        path: &["verda", "destroy_timeout_seconds"],
        kind: KnobKind::Float,
    },
    Knob {
        env: "VERDA_CREATE_RETRIES",
        path: &["verda", "create_retries"],
        kind: KnobKind::Int,
    },
];

/// Merge selected env knobs into the YAML tunables mapping.
pub(crate) fn apply_env_knobs(raw: &mut Value, env: &impl EnvSource) -> Result<(), ConfigError> {
    for knob in KNOBS {
        let Some(raw_val) = env.var(knob.env) else {
            continue;
        };
        let stripped = raw_val.trim();
        if stripped.is_empty() {
            continue;
        }
        let value = parse_knob(knob.env, stripped, knob.kind)?;
        set_path(raw, knob.path, value);
    }
    Ok(())
}

fn parse_knob(env: &str, stripped: &str, kind: KnobKind) -> Result<Value, ConfigError> {
    match kind {
        KnobKind::Bool => {
            let normalized = stripped.to_ascii_lowercase();
            if TRUE_VALUES.contains(&normalized.as_str()) {
                Ok(Value::Bool(true))
            } else if FALSE_VALUES.contains(&normalized.as_str()) {
                Ok(Value::Bool(false))
            } else {
                Err(ConfigError::invalid(format!(
                    "{env} must be a boolean (1/0, true/false, yes/no, or on/off): {stripped:?}"
                )))
            }
        }
        KnobKind::Str => Ok(Value::String(stripped.to_string())),
        KnobKind::Int => {
            let parsed: i64 = stripped.parse().map_err(|_| {
                ConfigError::invalid(format!("{env} must be an integer: {stripped:?}"))
            })?;
            Ok(Value::Number(parsed.into()))
        }
        KnobKind::Float => {
            let parsed: f64 = stripped.parse().map_err(|_| {
                ConfigError::invalid(format!("{env} must be a finite number: {stripped:?}"))
            })?;
            if !parsed.is_finite() {
                return Err(ConfigError::invalid(format!(
                    "{env} must be a finite number: {stripped:?}"
                )));
            }
            Ok(Value::Number(serde_yaml::Number::from(parsed)))
        }
        KnobKind::Csv => Ok(sequence(parse_csv(env, stripped)?)),
    }
}

fn parse_csv(env: &str, stripped: &str) -> Result<Vec<String>, ConfigError> {
    let items: Vec<String> = stripped
        .split(',')
        .map(|item| item.trim().to_string())
        .collect();
    if items.iter().any(String::is_empty) {
        return Err(ConfigError::invalid(format!(
            "{env} must be a comma-separated list without empty values: {stripped:?}"
        )));
    }
    Ok(items)
}

fn sequence(items: Vec<String>) -> Value {
    Value::Sequence(items.into_iter().map(Value::String).collect())
}

fn set_path(raw: &mut Value, path: &[&str], value: Value) {
    if path.is_empty() {
        return;
    }
    if !raw.is_mapping() {
        *raw = Value::Mapping(Mapping::new());
    }
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = raw;
    for key in parents {
        if !cursor.is_mapping() {
            return;
        }
        let mapping = match cursor {
            Value::Mapping(mapping) => mapping,
            _ => return,
        };
        if !mapping.contains_key(*key) || !mapping.get(*key).is_some_and(Value::is_mapping) {
            mapping.insert(
                Value::String((*key).to_string()),
                Value::Mapping(Mapping::new()),
            );
        }
        cursor = match mapping.get_mut(*key) {
            Some(next) => next,
            None => return,
        };
    }
    if let Value::Mapping(mapping) = cursor {
        mapping.insert(Value::String((*last).to_string()), value);
    }
}
