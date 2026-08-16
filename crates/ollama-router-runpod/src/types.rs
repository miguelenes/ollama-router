//! RunPod API DTOs. Unknown fields are ignored.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn de_opt_f64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
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

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CatalogPrice {
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub secure: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub community: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogDataCenter {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub availability: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogGpu {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    /// VRAM in GB.
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub memory: Option<f64>,
    #[serde(default)]
    pub price: Option<CatalogPrice>,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub data_centers: Vec<CatalogDataCenter>,
}

impl CatalogGpu {
    pub fn gpu_type_id(&self) -> Option<&str> {
        self.id
            .as_deref()
            .or(self.name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn is_available(&self) -> bool {
        match self.availability.as_deref().map(str::trim) {
            None => true,
            Some("") => true,
            Some(s) => !s.eq_ignore_ascii_case("NONE"),
        }
    }

    pub fn on_demand_price(&self, cloud_type: &str) -> Option<f64> {
        let price = self.price.as_ref()?;
        if cloud_type.eq_ignore_ascii_case("COMMUNITY") {
            price.community.or(price.secure)
        } else {
            price.secure.or(price.community)
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CatalogResponse {
    #[serde(default)]
    pub gpus: Vec<CatalogGpu>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodGpu {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub community_spot_price: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub secure_spot_price: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodMachine {
    #[serde(default)]
    pub data_center_id: Option<String>,
    #[serde(default)]
    pub gpu_type_id: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pod {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub desired_status: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub cost_per_hr: Option<f64>,
    #[serde(default)]
    pub interruptible: Option<bool>,
    #[serde(default)]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub gpu: Option<PodGpu>,
    #[serde(default)]
    pub machine: Option<PodMachine>,
}

impl Pod {
    pub fn pod_id(&self) -> Option<&str> {
        self.id.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn status(&self) -> &str {
        self.desired_status.as_deref().unwrap_or("")
    }

    pub fn cost_per_hour(&self) -> Option<f64> {
        self.cost_per_hr
    }

    pub fn data_center(&self) -> Option<&str> {
        self.machine
            .as_ref()
            .and_then(|m| m.data_center_id.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn gpu_type(&self) -> Option<&str> {
        self.machine
            .as_ref()
            .and_then(|m| m.gpu_type_id.as_deref())
            .or_else(|| self.gpu.as_ref().and_then(|g| g.id.as_deref()))
            .or_else(|| self.gpu.as_ref().and_then(|g| g.display_name.as_deref()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// Create-pod body. `Debug` redacts env values.
#[derive(Clone, Serialize)]
pub struct CreatePodRequest {
    pub name: String,
    pub image_name: String,
    pub interruptible: bool,
    pub cloud_type: String,
    pub gpu_type_ids: Vec<String>,
    pub gpu_type_priority: String,
    pub docker_start_cmd: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub container_disk_in_gb: u32,
    pub volume_in_gb: u32,
    pub ports: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_center_ids: Option<Vec<String>>,
}

impl std::fmt::Debug for CreatePodRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatePodRequest")
            .field("name", &self.name)
            .field("image_name", &self.image_name)
            .field("interruptible", &self.interruptible)
            .field("cloud_type", &self.cloud_type)
            .field("gpu_type_ids", &self.gpu_type_ids)
            .field("gpu_type_priority", &self.gpu_type_priority)
            .field("docker_start_cmd", &self.docker_start_cmd)
            .field(
                "env",
                &self
                    .env
                    .keys()
                    .map(|k| (k.as_str(), "REDACTED"))
                    .collect::<BTreeMap<_, _>>(),
            )
            .field("container_disk_in_gb", &self.container_disk_in_gb)
            .field("volume_in_gb", &self.volume_in_gb)
            .field("ports", &self.ports)
            .field("data_center_ids", &self.data_center_ids)
            .finish()
    }
}

impl CreatePodRequest {
    pub fn to_json(&self) -> Value {
        let mut body = serde_json::json!({
            "name": self.name,
            "imageName": self.image_name,
            "interruptible": self.interruptible,
            "cloudType": self.cloud_type,
            "gpuTypeIds": self.gpu_type_ids,
            "gpuTypePriority": self.gpu_type_priority,
            "dockerStartCmd": self.docker_start_cmd,
            "env": self.env,
            "containerDiskInGb": self.container_disk_in_gb,
            "volumeInGb": self.volume_in_gb,
            "ports": self.ports,
        });
        if let Some(dcs) = &self.data_center_ids {
            body["dataCenterIds"] = Value::Array(dcs.iter().cloned().map(Value::String).collect());
            body["dataCenterPriority"] = Value::String("custom".into());
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_request_debug_redacts_env() {
        let mut env = BTreeMap::new();
        env.insert("ZROK_ENABLE_TOKEN".into(), "secret-zrok".into());
        let req = CreatePodRequest {
            name: "or-rp-test".into(),
            image_name: "img".into(),
            interruptible: true,
            cloud_type: "SECURE".into(),
            gpu_type_ids: vec!["NVIDIA L4".into()],
            gpu_type_priority: "custom".into(),
            docker_start_cmd: vec!["bash".into(), "-lc".into(), "echo hi".into()],
            env,
            container_disk_in_gb: 40,
            volume_in_gb: 0,
            ports: vec![],
            data_center_ids: None,
        };
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("secret-zrok"), "{dbg}");
        assert!(dbg.contains("REDACTED"), "{dbg}");
    }

    #[test]
    fn pod_ignores_unknown_fields() {
        let pod: Pod = serde_json::from_value(serde_json::json!({
            "id": "pod-1",
            "name": "or-rp-x",
            "desiredStatus": "RUNNING",
            "costPerHr": 0.42,
            "futureField": {"nested": true},
        }))
        .unwrap();
        assert_eq!(pod.pod_id(), Some("pod-1"));
        assert_eq!(pod.status(), "RUNNING");
        assert_eq!(pod.cost_per_hour(), Some(0.42));
    }
}
