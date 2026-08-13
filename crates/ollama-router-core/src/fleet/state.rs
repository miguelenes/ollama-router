//! Durable fleet-state store: remembered Tailscale URLs and Verda metadata.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fleet::ids::{NodeId, VerdaInstanceId};
use crate::fleet::tailscale::{is_tailscale_ipv4, routing_url_from_fields, url_host_is_tailscale};

/// Default on-disk location (override with `OLLAMA_ROUTER_STATE_FILE`).
pub const DEFAULT_STATE_PATH: &str = "/var/lib/ollama-router/fleet-state.json";

/// Raised when neither durable copy can be read safely.
#[derive(Debug, thiserror::Error)]
pub enum FleetStateError {
    /// A single copy is not valid JSON or is unreadable.
    #[error("invalid fleet state file: {path}")]
    InvalidFile { path: PathBuf },
    /// JSON was valid but not a mapping of node id → object.
    #[error("fleet state file must be a mapping of node ids to objects: {path}")]
    InvalidShape { path: PathBuf },
    /// Primary and backup are both unusable.
    #[error("fleet state is unreadable at {path}; backup {backup} is also unreadable")]
    Unreadable { path: PathBuf, backup: PathBuf },
    /// Exclusive lock could not be taken.
    #[error("fleet state lock failed: {0}")]
    Lock(io::Error),
    /// Filesystem error while writing.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Fields written by [`FleetState::persist_verda_node`].
#[derive(Clone, Debug)]
pub struct VerdaNodePersist<'a> {
    /// Ollama base URL (Tailscale preferred; public IPv4 will not clobber).
    pub url: &'a str,
    /// Cloud instance id.
    pub instance_id: &'a VerdaInstanceId,
    /// Verda location code.
    pub location: &'a str,
    /// Instance type slug.
    pub instance_type: &'a str,
    /// Optional OS volume id.
    pub os_volume_id: Option<&'a str>,
    /// Optional Tailscale CGNAT address.
    pub tailscale_ip: Option<&'a str>,
    /// Spot price in currency per hour, when known.
    pub spot_price_per_hour: Option<f64>,
}

/// One host entry. Unknown JSON keys round-trip via [`Self::extra`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetStateEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verda_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verda_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verda_instance_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verda_os_volume_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verda_spot_price_per_hour: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Key-value store per host id with flock-locked atomic writes.
#[derive(Debug, Clone)]
pub struct FleetState {
    path: PathBuf,
}

impl FleetState {
    /// Create a store pointing at `path`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    fn lock_path(&self) -> PathBuf {
        append_suffix(&self.path, ".lock")
    }

    fn backup_path(&self) -> PathBuf {
        append_suffix(&self.path, ".bak")
    }

    /// Read primary state, recover from backup, or fail closed.
    pub fn load(&self) -> Result<BTreeMap<String, FleetStateEntry>, FleetStateError> {
        let primary_exists = self.path.exists();
        let backup_exists = self.backup_path().exists();
        if !primary_exists && !backup_exists {
            return Ok(BTreeMap::new());
        }
        match read_state_file(&self.path) {
            Ok(data) => Ok(data),
            Err(_) => match read_state_file(&self.backup_path()) {
                Ok(data) => Ok(data),
                Err(_) => Err(FleetStateError::Unreadable {
                    path: self.path.clone(),
                    backup: self.backup_path(),
                }),
            },
        }
    }

    /// Return a Tailscale routing URL for `node_id`, or `None`.
    pub fn hydrate_url(&self, node_id: &NodeId) -> Result<Option<String>, FleetStateError> {
        let data = self.load()?;
        Ok(data.get(node_id.as_str()).and_then(|entry| {
            routing_url_from_fields(entry.url.as_deref(), entry.tailscale_ip.as_deref())
        }))
    }

    /// Write or update routing fields. Non-Tailscale URLs never clobber an
    /// existing Tailscale routing URL.
    pub fn persist_url(
        &self,
        node_id: impl AsRef<str>,
        url: &str,
        tailscale_ip: Option<&str>,
    ) -> Result<(), FleetStateError> {
        let _lock = self.lock_exclusive()?;
        let mut data = self.load()?;
        let existing = data.get(node_id.as_ref()).cloned().unwrap_or_default();
        let mut entry = merge_routing_fields(&existing, url, tailscale_ip);
        entry.updated_at = Some(now_secs());
        data.insert(node_id.as_ref().to_string(), entry);
        atomic_write(&self.path, &data)?;
        Ok(())
    }

    /// Drop a host entry.
    pub fn remove(&self, node_id: impl AsRef<str>) -> Result<(), FleetStateError> {
        let _lock = self.lock_exclusive()?;
        let mut data = self.load()?;
        if data.remove(node_id.as_ref()).is_some() {
            atomic_write(&self.path, &data)?;
        }
        Ok(())
    }

    /// Return the raw entry for `node_id`.
    pub fn get_entry(
        &self,
        node_id: impl AsRef<str>,
    ) -> Result<Option<FleetStateEntry>, FleetStateError> {
        let data = self.load()?;
        Ok(data.get(node_id.as_ref()).cloned())
    }

    /// Persist a Verda spot node (URL + instance metadata + optional spot price).
    pub fn persist_verda_node(
        &self,
        node_id: impl AsRef<str>,
        persist: VerdaNodePersist<'_>,
    ) -> Result<(), FleetStateError> {
        let _lock = self.lock_exclusive()?;
        let mut data = self.load()?;
        let existing = data.get(node_id.as_ref()).cloned().unwrap_or_default();
        let mut entry = merge_routing_fields(&existing, persist.url, persist.tailscale_ip);
        entry.updated_at = Some(now_secs());
        entry.managed_by = Some("verda".to_string());
        entry.verda_instance_id = Some(persist.instance_id.as_str().to_string());
        entry.verda_location = Some(persist.location.to_string());
        entry.verda_instance_type = Some(persist.instance_type.to_string());
        if let Some(vol) = persist.os_volume_id {
            entry.verda_os_volume_id = Some(vol.to_string());
        }
        if let Some(price) = persist.spot_price_per_hour {
            entry.verda_spot_price_per_hour = Some(price);
        }
        tracing::info!(
            node_id = %node_id.as_ref(),
            instance_type = persist.instance_type,
            location = persist.location,
            spot_price = persist.spot_price_per_hour,
            "verda persist"
        );
        data.insert(node_id.as_ref().to_string(), entry);
        atomic_write(&self.path, &data)?;
        Ok(())
    }

    /// Entries whose `managed_by` is `verda`.
    pub fn list_verda_nodes(&self) -> Result<BTreeMap<String, FleetStateEntry>, FleetStateError> {
        let data = self.load()?;
        Ok(data
            .into_iter()
            .filter(|(_, e)| e.managed_by.as_deref() == Some("verda"))
            .collect())
    }

    fn lock_exclusive(&self) -> Result<File, FleetStateError> {
        if let Some(parent) = self.lock_path().parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;
        FileExt::lock(&file).map_err(FleetStateError::Lock)?;
        Ok(file)
    }
}

fn merge_routing_fields(
    existing: &FleetStateEntry,
    url: &str,
    tailscale_ip: Option<&str>,
) -> FleetStateEntry {
    let mut entry = existing.clone();
    let existing_safe =
        routing_url_from_fields(existing.url.as_deref(), existing.tailscale_ip.as_deref());
    let incoming_ts = !url.trim().is_empty() && url_host_is_tailscale(url);
    let incoming_ts_ip = tailscale_ip
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some_and(is_tailscale_ipv4);

    if incoming_ts {
        entry.url = Some(url.trim().trim_end_matches('/').to_string());
    } else if let Some(safe) = existing_safe {
        entry.url = Some(safe);
    } else if !url.trim().is_empty() {
        entry.url = Some(url.trim().trim_end_matches('/').to_string());
    }

    if incoming_ts_ip {
        if let Some(ip) = tailscale_ip {
            entry.tailscale_ip = Some(ip.trim().to_string());
        }
    } else if !existing
        .tailscale_ip
        .as_deref()
        .is_some_and(is_tailscale_ipv4)
    {
        entry.tailscale_ip = None;
    }
    entry
}

fn read_state_file(path: &Path) -> Result<BTreeMap<String, FleetStateEntry>, FleetStateError> {
    let text = fs::read_to_string(path).map_err(|_| FleetStateError::InvalidFile {
        path: path.to_path_buf(),
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|_| FleetStateError::InvalidFile {
        path: path.to_path_buf(),
    })?;
    let Value::Object(map) = value else {
        return Err(FleetStateError::InvalidShape {
            path: path.to_path_buf(),
        });
    };
    let mut out = BTreeMap::new();
    for (key, val) in map {
        if !val.is_object() {
            return Err(FleetStateError::InvalidShape {
                path: path.to_path_buf(),
            });
        }
        let entry: FleetStateEntry =
            serde_json::from_value(val).map_err(|_| FleetStateError::InvalidShape {
                path: path.to_path_buf(),
            })?;
        out.insert(key, entry);
    }
    Ok(out)
}

fn atomic_write(
    path: &Path,
    data: &BTreeMap<String, FleetStateEntry>,
) -> Result<(), FleetStateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = append_suffix(path, ".tmp");
    let backup = append_suffix(path, ".bak");
    let backup_tmp = append_suffix(&backup, ".tmp");
    let payload = serde_json::to_vec_pretty(data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let write_result = (|| -> Result<(), FleetStateError> {
        write_file_fsync(&tmp, &payload)?;

        if path.is_file() && read_state_file(path).is_ok() {
            let current = fs::read(path)?;
            write_file_fsync(&backup_tmp, &current)?;
            fs::rename(&backup_tmp, &backup)?;
            let _ = fsync_directory(path.parent().unwrap_or(Path::new(".")));
        }

        fs::rename(&tmp, path)?;
        let _ = fsync_directory(path.parent().unwrap_or(Path::new(".")));

        if !backup.exists() {
            write_file_fsync(&backup_tmp, &payload)?;
            fs::rename(&backup_tmp, &backup)?;
            let _ = fsync_directory(path.parent().unwrap_or(Path::new(".")));
        }
        Ok(())
    })();

    let _ = fs::remove_file(&tmp);
    let _ = fs::remove_file(&backup_tmp);
    write_result
}

fn write_file_fsync(path: &Path, payload: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(payload)?;
    file.sync_all()?;
    Ok(())
}

fn fsync_directory(path: &Path) -> io::Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()
}

fn append_suffix(path: &Path, extra: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(extra);
    PathBuf::from(raw)
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::ids::VerdaInstanceId;

    #[test]
    fn load_empty_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        assert!(state.load().unwrap().is_empty());
    }

    #[test]
    fn load_recovers_backup_when_primary_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let state = FleetState::new(&path);
        state
            .persist_url("nuc", "http://100.100.14.5:11434", Some("100.100.14.5"))
            .unwrap();
        fs::write(&path, "{ invalid json {{{").unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(
            loaded["nuc"].url.as_deref(),
            Some("http://100.100.14.5:11434")
        );
    }

    #[test]
    fn load_fails_closed_when_both_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let state = FleetState::new(&path);
        fs::write(&path, "[1, 2, 3]").unwrap();
        fs::write(append_suffix(&path, ".bak"), "{ invalid json {{{").unwrap();
        let err = state.load().unwrap_err();
        assert!(matches!(err, FleetStateError::Unreadable { .. }));
    }

    #[test]
    fn load_rejects_invalid_entry_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let state = FleetState::new(&path);
        fs::write(&path, r#"{"node": "not-an-object"}"#).unwrap();
        assert!(matches!(
            state.load().unwrap_err(),
            FleetStateError::Unreadable { .. }
        ));
    }

    #[test]
    fn persist_and_hydrate() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        state
            .persist_url("nuc", "http://100.100.14.5:11434", Some("100.100.14.5"))
            .unwrap();
        let id = NodeId::parse("nuc").unwrap();
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://100.100.14.5:11434")
        );
        assert!(state
            .hydrate_url(&NodeId::parse("bogus").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn public_url_does_not_clobber_tailscale() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        state
            .persist_url("verda-1", "http://100.64.0.1:11434", Some("100.64.0.1"))
            .unwrap();
        let instance = VerdaInstanceId::parse("i-1").unwrap();
        state
            .persist_verda_node(
                "verda-1",
                VerdaNodePersist {
                    url: "http://135.181.1.1:11434",
                    instance_id: &instance,
                    location: "HEL1",
                    instance_type: "gpu",
                    os_volume_id: None,
                    tailscale_ip: None,
                    spot_price_per_hour: Some(0.42),
                },
            )
            .unwrap();
        let id = NodeId::parse("verda-1").unwrap();
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://100.64.0.1:11434")
        );
        let entry = state.get_entry("verda-1").unwrap().unwrap();
        assert_eq!(entry.tailscale_ip.as_deref(), Some("100.64.0.1"));
        assert_eq!(entry.verda_instance_id.as_deref(), Some("i-1"));
        assert_eq!(entry.verda_spot_price_per_hour, Some(0.42));
        assert_eq!(entry.managed_by.as_deref(), Some("verda"));
    }

    #[test]
    fn multiple_hosts_hydrate_only_tailscale() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        state.persist_url("a", "http://a:11434", None).unwrap();
        state.persist_url("b", "http://b:11434", None).unwrap();
        state
            .persist_url("c", "http://c:11434", Some("100.64.0.1"))
            .unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded["a"].url.as_deref(), Some("http://a:11434"));
        assert!(state
            .hydrate_url(&NodeId::parse("a").unwrap())
            .unwrap()
            .is_none());
        assert_eq!(
            state
                .hydrate_url(&NodeId::parse("c").unwrap())
                .unwrap()
                .as_deref(),
            Some("http://100.64.0.1:11434")
        );
    }

    #[test]
    fn remove_and_tmp_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let tmp = append_suffix(&path, ".tmp");
        let state = FleetState::new(&path);
        state.persist_url("a", "http://a:11434", None).unwrap();
        state.persist_url("b", "http://b:11434", None).unwrap();
        state.remove("a").unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key("b"));
        state.remove("nope").unwrap();
        assert!(!tmp.exists());
        assert!(path.is_file());
    }

    #[test]
    fn leftover_unknown_keys_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        fs::write(
            &path,
            r#"{"n":{"url":"http://100.64.0.1:11434","thunder_instance_id":"old"}}"#,
        )
        .unwrap();
        let state = FleetState::new(&path);
        let entry = state.get_entry("n").unwrap().unwrap();
        assert_eq!(
            entry
                .extra
                .get("thunder_instance_id")
                .and_then(Value::as_str),
            Some("old")
        );
        state
            .persist_url("n", "http://100.64.0.1:11434", Some("100.64.0.1"))
            .unwrap();
        let again = state.get_entry("n").unwrap().unwrap();
        assert_eq!(
            again
                .extra
                .get("thunder_instance_id")
                .and_then(Value::as_str),
            Some("old")
        );
    }
}
