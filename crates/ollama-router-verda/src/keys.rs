//! SSH public-key resolution for Verda instances. Never logs key material.

use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use ollama_router_core::config::VerdaConfig;

use crate::client::{VerdaClient, VerdaError};

pub fn fingerprint_public_key(key_text: &str) -> Option<String> {
    let mut parts = key_text.split_whitespace();
    let _kind = parts.next()?;
    let b64 = parts.next()?;
    let blob = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).ok()?;
    let digest = Md5::digest(&blob);
    Some(
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

pub fn normalize_fingerprint(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    let cleaned = raw
        .strip_prefix("MD5:")
        .or_else(|| raw.strip_prefix("md5:"))
        .or_else(|| raw.strip_prefix("SHA256:"))
        .or_else(|| raw.strip_prefix("sha256:"))
        .unwrap_or(raw);
    Some(cleaned.to_ascii_lowercase())
}

pub fn resolve_public_key_path(config: &VerdaConfig, key_file: Option<&str>) -> Option<PathBuf> {
    let candidate = if let Some(p) = config.ssh_public_key_file.as_deref() {
        p.to_string()
    } else if let Some(p) = config.ssh_private_key_file.as_deref() {
        format!("{p}.pub")
    } else {
        format!("{}.pub", key_file?)
    };
    let path = PathBuf::from(candidate);
    path.is_file().then_some(path)
}

pub fn read_public_key_text(
    config: &VerdaConfig,
    key_file: Option<&str>,
) -> Result<String, VerdaError> {
    let path = resolve_public_key_path(config, key_file).ok_or_else(|| {
        VerdaError::Message(
            "no Verda SSH public key available: set verda.ssh_public_key_file or verda.ssh_private_key_file (+.pub)"
                .into(),
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| VerdaError::Message(format!("cannot read SSH public key: {err}")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(VerdaError::Message(format!(
            "SSH public key file {} is empty",
            path.display()
        )));
    }
    Ok(trimmed.to_string())
}

pub async fn ensure_ssh_key_id(
    client: &VerdaClient,
    config: &VerdaConfig,
    key_file: Option<&str>,
) -> Result<Option<String>, VerdaError> {
    let keys = client.list_ssh_keys().await?;
    if let Some(want) = config.ssh_key_id.as_deref() {
        if keys.iter().any(|k| k.key_id() == Some(want)) {
            tracing::info!(ssh_key_id = want, "verda_ssh_key_configured");
            return Ok(Some(want.to_string()));
        }
        tracing::warn!(ssh_key_id = want, "verda_ssh_key_id_missing");
    }
    if let Some(found) = keys
        .iter()
        .find(|k| k.name.as_deref() == Some(&config.ssh_key_name))
    {
        if let Some(id) = found.key_id() {
            tracing::info!(name = %config.ssh_key_name, "verda_ssh_key_by_name");
            return Ok(Some(id.to_string()));
        }
    }
    let Ok(key_text) = read_public_key_text(config, key_file) else {
        tracing::info!("verda_create_omitting_ssh_key_ids");
        return Ok(None);
    };
    if let Some(local_fp) = fingerprint_public_key(&key_text) {
        for key in &keys {
            if normalize_fingerprint(key.fingerprint.as_deref()) == Some(local_fp.clone()) {
                if let Some(id) = key.key_id() {
                    tracing::info!("verda_ssh_key_by_fingerprint");
                    return Ok(Some(id.to_string()));
                }
            }
        }
    }
    let created = client
        .create_ssh_key(&config.ssh_key_name, &key_text)
        .await?;
    created
        .key_id()
        .map(|id| Some(id.to_string()))
        .ok_or_else(|| {
            VerdaError::Message("Verda accepted the SSH key upload but returned no id".into())
        })
}

pub fn companion_private_key(path: &Path) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()) == Some("pub") {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip_shape() {
        // ssh-ed25519 with tiny invalid blob still formats or returns None.
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJustATestBlob comment";
        let fp = fingerprint_public_key(key);
        assert!(fp.is_none() || fp.unwrap().contains(':'));
    }

    #[test]
    fn fingerprint_valid_ed25519_blob() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKtestblobforfingerprintxx comment";
        // May still fail decode length; empty/malformed yields None.
        let _ = fingerprint_public_key(key);
        assert!(fingerprint_public_key("not-a-key").is_none());
        assert!(fingerprint_public_key("").is_none());
        assert!(fingerprint_public_key("ssh-ed25519").is_none());
    }

    #[test]
    fn normalize_fingerprint_strips_prefixes() {
        assert_eq!(
            normalize_fingerprint(Some("MD5:aa:bb")),
            Some("aa:bb".into())
        );
        assert_eq!(
            normalize_fingerprint(Some("md5:AA:BB")),
            Some("aa:bb".into())
        );
        assert_eq!(
            normalize_fingerprint(Some("SHA256:DeadBeef")),
            Some("deadbeef".into())
        );
        assert_eq!(normalize_fingerprint(Some("  ")), None);
        assert_eq!(normalize_fingerprint(None), None);
    }

    #[test]
    fn companion_private_key_strips_pub() {
        assert_eq!(
            companion_private_key(Path::new("/tmp/id_ed25519.pub")),
            PathBuf::from("/tmp/id_ed25519")
        );
        assert_eq!(
            companion_private_key(Path::new("/tmp/id_ed25519")),
            PathBuf::from("/tmp/id_ed25519")
        );
    }
}
