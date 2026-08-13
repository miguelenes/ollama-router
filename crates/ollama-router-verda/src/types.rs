//! Verda API DTOs. Unknown fields are ignored.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn de_opt_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => {
            let digits: String = s
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            digits.parse().ok()
        }
        Some(_) => None,
    })
}

fn de_opt_stringish<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string().trim_matches('"').to_string()),
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InstanceAvailability {
    pub location_code: String,
    #[serde(default)]
    pub availabilities: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GpuSpec {
    #[serde(default)]
    pub number_of_gpus: Option<u32>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl GpuSpec {
    pub fn gpu_count(&self) -> u32 {
        self.number_of_gpus.unwrap_or(0)
    }

    pub fn is_nvidia(&self) -> bool {
        [&self.manufacturer, &self.model, &self.name]
            .into_iter()
            .flatten()
            .any(|s| s.to_ascii_uppercase().contains("NVIDIA"))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GpuMemorySpec {
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub size_in_gigabytes: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InstanceType {
    pub instance_type: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default, deserialize_with = "de_opt_stringish")]
    pub spot_price: Option<String>,
    #[serde(default, deserialize_with = "de_opt_stringish")]
    pub price_per_hour: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub gpu: Option<GpuSpec>,
    #[serde(default)]
    pub gpu_memory: Option<GpuMemorySpec>,
    #[serde(default)]
    pub supported_os: Vec<String>,
}

impl InstanceType {
    pub fn spot_price_float(&self) -> Option<f64> {
        self.spot_price.as_ref()?.parse().ok()
    }

    pub fn vram_gb(&self) -> Option<f64> {
        self.gpu_memory.as_ref()?.size_in_gigabytes
    }

    pub fn gpu_count(&self) -> u32 {
        if let Some(gpu) = &self.gpu {
            if gpu.gpu_count() > 0 {
                return gpu.gpu_count();
            }
        }
        if self.is_nvidia_gpu() {
            1
        } else {
            0
        }
    }

    pub fn is_nvidia_gpu(&self) -> bool {
        if self.gpu.as_ref().is_some_and(GpuSpec::is_nvidia) {
            return true;
        }
        self.manufacturer
            .as_deref()
            .is_some_and(|m| m.to_ascii_uppercase().contains("NVIDIA"))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Image {
    pub image_type: String,
    #[serde(default)]
    pub image_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SshKey {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub ssh_key_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

impl SshKey {
    pub fn key_id(&self) -> Option<&str> {
        self.id.as_deref().or(self.ssh_key_id.as_deref())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Tag {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Instance {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub ip_address: Option<String>,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub location_code: Option<String>,
    #[serde(default)]
    pub instance_type: Option<String>,
    #[serde(default)]
    pub os_volume_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

impl Instance {
    pub fn instance_id_value(&self) -> Option<&str> {
        self.id.as_deref().or(self.instance_id.as_deref())
    }

    pub fn public_ip_value(&self) -> Option<&str> {
        self.ip
            .as_deref()
            .or(self.ip_address.as_deref())
            .or(self.public_ip.as_deref())
    }

    pub fn location_value(&self) -> Option<&str> {
        self.location.as_deref().or(self.location_code.as_deref())
    }

    pub fn tag_map(&self) -> std::collections::BTreeMap<&str, &str> {
        self.tags
            .iter()
            .map(|t| (t.key.as_str(), t.value.as_str()))
            .collect()
    }
}
