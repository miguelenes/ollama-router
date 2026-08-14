//! Durable fleet-state store: remembered overlay URLs and Verda metadata.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fleet::ids::{NodeId, VerdaInstanceId};
use crate::fleet::url_policy::{url_host_is_loopback, url_is_safe_overlay};

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

impl FleetStateError {
    pub(crate) fn is_permission_denied(&self) -> bool {
        match self {
            Self::Io(err) | Self::Lock(err) => err.kind() == io::ErrorKind::PermissionDenied,
            _ => false,
        }
    }
}

/// Fields written by [`FleetState::persist_enroll`]. Share ids, not enable tokens.
#[derive(Clone, Debug)]
pub struct EnrollPersist<'a> {
    /// Loopback zrok access URL for Ollama (`http://127.0.0.1:PORT`).
    pub url: &'a str,
    /// Loopback zrok access URL for the node-agent.
    pub capacity_url: &'a str,
    /// zrok private share unique-name for Ollama.
    pub ollama_share_id: &'a str,
    /// zrok private share unique-name for the agent.
    pub agent_share_id: &'a str,
}

/// Fields written by [`FleetState::persist_verda_node`].
#[derive(Clone, Debug)]
pub struct VerdaNodePersist<'a> {
    /// Ollama base URL (overlay/loopback preferred; public IPv4 will not clobber).
    pub url: &'a str,
    /// Cloud instance id.
    pub instance_id: &'a VerdaInstanceId,
    /// Verda location code.
    pub location: &'a str,
    /// Instance type slug.
    pub instance_type: &'a str,
    /// Optional OS volume id.
    pub os_volume_id: Option<&'a str>,
    /// Spot price in currency per hour, when known.
    pub spot_price_per_hour: Option<f64>,
    /// Hostname assigned at create (guest enroll may key off this).
    pub hostname: Option<&'a str>,
}

/// One host entry. Unknown JSON keys round-trip via [`Self::extra`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetStateEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Private share unique-name (Ollama). Unknown legacy keys land in [`Self::extra`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_token_id: Option<String>,
    /// Loopback zrok access URL (`http://127.0.0.1:PORT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_access_url: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_share_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_share_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_backend: Option<String>,
    /// Guest hostname set on Verda create (enroll may send this as `id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
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

    /// Create parent dirs and an empty `{}` mapping if neither primary nor backup exists.
    ///
    /// No-op when a file is already present. Callers that cannot write the
    /// default `/var/lib/ollama-router` path should treat permission errors as
    /// "stay in-memory empty" rather than failing startup.
    pub fn ensure_created(&self) -> Result<(), FleetStateError> {
        if self.path.exists() || self.backup_path().exists() {
            return Ok(());
        }
        let _lock = self.lock_exclusive()?;
        if self.path.exists() || self.backup_path().exists() {
            return Ok(());
        }
        atomic_write(&self.path, &BTreeMap::new())?;
        Ok(())
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

    /// Return a routing URL for `node_id`: loopback enroll, RFC1918, or hostname.
    ///
    /// Legacy CGNAT / unknown overlay keys in `extra` are ignored until re-enroll.
    pub fn hydrate_url(&self, node_id: &NodeId) -> Result<Option<String>, FleetStateError> {
        let data = self.load()?;
        Ok(data.get(node_id.as_str()).and_then(hydrate_entry_url))
    }

    /// Return a persisted capacity-agent URL (zrok loopback enroll).
    pub fn hydrate_capacity_url(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<String>, FleetStateError> {
        let data = self.load()?;
        Ok(data.get(node_id.as_str()).and_then(|entry| {
            entry
                .capacity_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter(|url| url_is_safe_overlay(url))
                .map(|s| s.trim_end_matches('/').to_string())
        }))
    }

    /// Write or update routing fields. Public IPv4 never clobbers a good overlay.
    pub fn persist_url(&self, node_id: impl AsRef<str>, url: &str) -> Result<(), FleetStateError> {
        let _lock = self.lock_exclusive()?;
        let mut data = self.load()?;
        let existing = data.get(node_id.as_ref()).cloned().unwrap_or_default();
        let mut entry = merge_routing_fields(&existing, url);
        entry.updated_at = Some(now_secs());
        data.insert(node_id.as_ref().to_string(), entry);
        atomic_write(&self.path, &data)?;
        Ok(())
    }

    /// Persist zrok enroll reachability. Does not change `managed_by`.
    ///
    /// Never writes `fleet.yaml`. Share ids only — not enable tokens.
    pub fn persist_enroll(
        &self,
        node_id: impl AsRef<str>,
        persist: EnrollPersist<'_>,
    ) -> Result<(), FleetStateError> {
        let _lock = self.lock_exclusive()?;
        let mut data = self.load()?;
        let mut entry = data.get(node_id.as_ref()).cloned().unwrap_or_default();
        let url = persist.url.trim().trim_end_matches('/').to_string();
        entry.url = Some(url.clone());
        entry.local_access_url = Some(url);
        entry.share_token_id = Some(persist.ollama_share_id.trim().to_string());
        entry.capacity_url = Some(
            persist
                .capacity_url
                .trim()
                .trim_end_matches('/')
                .to_string(),
        );
        entry.ollama_share_id = Some(persist.ollama_share_id.trim().to_string());
        entry.agent_share_id = Some(persist.agent_share_id.trim().to_string());
        entry.tunnel_backend = Some("zrok".to_string());
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
        let mut entry = merge_routing_fields(&existing, persist.url);
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
        if let Some(host) = persist.hostname.map(str::trim).filter(|s| !s.is_empty()) {
            entry.hostname = Some(host.to_string());
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

fn hydrate_entry_url(entry: &FleetStateEntry) -> Option<String> {
    for candidate in [entry.local_access_url.as_deref(), entry.url.as_deref()] {
        if let Some(url) = candidate
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|url| url_is_safe_overlay(url))
        {
            return Some(url.trim_end_matches('/').to_string());
        }
    }
    None
}

fn merge_routing_fields(existing: &FleetStateEntry, url: &str) -> FleetStateEntry {
    let mut entry = existing.clone();
    let incoming = url.trim().trim_end_matches('/');
    let incoming_safe = !incoming.is_empty() && url_is_safe_overlay(incoming);
    let existing_safe = hydrate_entry_url(existing);

    if incoming_safe {
        entry.url = Some(incoming.to_string());
        if url_host_is_loopback(incoming) {
            entry.local_access_url = Some(incoming.to_string());
        }
    } else if existing_safe.is_some() {
        entry.url = existing.url.clone();
        entry.local_access_url = existing.local_access_url.clone();
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
    fn ensure_created_writes_empty_object_once() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("state").join("fleet-state.json");
        let state = FleetState::new(&nested);
        assert!(!nested.exists());
        state.ensure_created().unwrap();
        let value: Value = serde_json::from_str(&fs::read_to_string(&nested).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({}));
        fs::write(&nested, r#"{"kept":{}}"#).unwrap();
        state.ensure_created().unwrap();
        assert!(fs::read_to_string(&nested).unwrap().contains("kept"));
    }

    #[test]
    fn load_recovers_backup_when_primary_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let state = FleetState::new(&path);
        state.persist_url("nuc", "http://10.0.0.5:11434").unwrap();
        fs::write(&path, "{ invalid json {{{").unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(loaded["nuc"].url.as_deref(), Some("http://10.0.0.5:11434"));
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
        state.persist_url("nuc", "http://10.0.0.5:11434").unwrap();
        let id = NodeId::parse("nuc").unwrap();
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://10.0.0.5:11434")
        );
        assert!(state
            .hydrate_url(&NodeId::parse("bogus").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn public_url_does_not_clobber_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        state
            .persist_url("verda-1", "http://127.0.0.1:41990")
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
                    spot_price_per_hour: Some(0.42),
                    hostname: None,
                },
            )
            .unwrap();
        let id = NodeId::parse("verda-1").unwrap();
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://127.0.0.1:41990")
        );
        let entry = state.get_entry("verda-1").unwrap().unwrap();
        assert_eq!(
            entry.local_access_url.as_deref(),
            Some("http://127.0.0.1:41990")
        );
        assert_eq!(entry.verda_instance_id.as_deref(), Some("i-1"));
        assert_eq!(entry.verda_spot_price_per_hour, Some(0.42));
        assert_eq!(entry.managed_by.as_deref(), Some("verda"));
    }

    #[test]
    fn cgnat_is_not_a_routing_url() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        state.persist_url("a", "http://a:11434").unwrap();
        state.persist_url("b", "http://b:11434").unwrap();
        state.persist_url("c", "http://100.64.0.1:11434").unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded["a"].url.as_deref(), Some("http://a:11434"));
        assert_eq!(
            state
                .hydrate_url(&NodeId::parse("a").unwrap())
                .unwrap()
                .as_deref(),
            Some("http://a:11434")
        );
        assert!(state
            .hydrate_url(&NodeId::parse("c").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn old_tailscale_ip_is_ignored_until_re_enroll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        fs::write(
            &path,
            r#"{"n":{"url":"http://100.64.0.1:11434","tailscale_ip":"100.64.0.1"}}"#,
        )
        .unwrap();
        let state = FleetState::new(&path);
        let id = NodeId::parse("n").unwrap();
        assert!(state.hydrate_url(&id).unwrap().is_none());
        let entry = state.get_entry("n").unwrap().unwrap();
        assert_eq!(
            entry.extra.get("tailscale_ip").and_then(Value::as_str),
            Some("100.64.0.1")
        );
    }

    #[test]
    fn remove_and_tmp_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-state.json");
        let tmp = append_suffix(&path, ".tmp");
        let state = FleetState::new(&path);
        state.persist_url("a", "http://a:11434").unwrap();
        state.persist_url("b", "http://b:11434").unwrap();
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
            r#"{"n":{"url":"http://127.0.0.1:41990","thunder_instance_id":"old"}}"#,
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
        state.persist_url("n", "http://127.0.0.1:41990").unwrap();
        let again = state.get_entry("n").unwrap().unwrap();
        assert_eq!(
            again
                .extra
                .get("thunder_instance_id")
                .and_then(Value::as_str),
            Some("old")
        );
    }

    #[test]
    fn persist_enroll_hydrates_loopback_and_updates_verda_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let state = FleetState::new(dir.path().join("fleet-state.json"));
        let instance = VerdaInstanceId::parse("i-1").unwrap();
        state
            .persist_verda_node(
                "verda-i-1",
                VerdaNodePersist {
                    url: "",
                    instance_id: &instance,
                    location: "HEL1",
                    instance_type: "gpu",
                    os_volume_id: None,
                    spot_price_per_hour: None,
                    hostname: None,
                },
            )
            .unwrap();
        state
            .persist_enroll(
                "verda-i-1",
                EnrollPersist {
                    url: "http://127.0.0.1:41990",
                    capacity_url: "http://127.0.0.1:41991",
                    ollama_share_id: "share-ollama",
                    agent_share_id: "share-agent",
                },
            )
            .unwrap();
        let loaded = state.load().unwrap();
        assert_eq!(loaded.len(), 1);
        let entry = &loaded["verda-i-1"];
        assert_eq!(entry.managed_by.as_deref(), Some("verda"));
        assert_eq!(entry.verda_instance_id.as_deref(), Some("i-1"));
        assert_eq!(entry.tunnel_backend.as_deref(), Some("zrok"));
        assert_eq!(entry.url.as_deref(), Some("http://127.0.0.1:41990"));
        assert_eq!(
            entry.local_access_url.as_deref(),
            Some("http://127.0.0.1:41990")
        );
        assert_eq!(entry.share_token_id.as_deref(), Some("share-ollama"));
        assert_eq!(
            entry.capacity_url.as_deref(),
            Some("http://127.0.0.1:41991")
        );
        let id = NodeId::parse("verda-i-1").unwrap();
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://127.0.0.1:41990")
        );
        assert_eq!(
            state.hydrate_capacity_url(&id).unwrap().as_deref(),
            Some("http://127.0.0.1:41991")
        );

        state
            .persist_verda_node(
                "verda-i-1",
                VerdaNodePersist {
                    url: "http://135.181.1.1:11434",
                    instance_id: &instance,
                    location: "HEL1",
                    instance_type: "gpu",
                    os_volume_id: None,
                    spot_price_per_hour: None,
                    hostname: None,
                },
            )
            .unwrap();
        assert_eq!(state.load().unwrap().len(), 1);
        assert_eq!(
            state.hydrate_url(&id).unwrap().as_deref(),
            Some("http://127.0.0.1:41990")
        );
    }
}
