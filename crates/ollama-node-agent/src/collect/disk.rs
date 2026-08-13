//! Models-dir filesystem inventory via sysinfo disks.

use std::path::{Path, PathBuf};

use ollama_capacity_types::bytes_to_gib;
use sysinfo::Disks;

/// Default Ollama models directory when config omits `models_dir`.
pub fn default_models_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join(".ollama").join("models");
        }
        return PathBuf::from(r"C:\Users\Default\.ollama\models");
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user = PathBuf::from(home).join(".ollama").join("models");
        if user.exists() || !cfg!(target_os = "linux") {
            return user;
        }
    }
    if cfg!(target_os = "linux") {
        let system = PathBuf::from("/usr/share/ollama/.ollama/models");
        if system.exists() {
            return system;
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".ollama").join("models");
        }
    }
    PathBuf::from("/usr/share/ollama/.ollama/models")
}

/// `(total_gb, available_gb)` for the mount that contains `models_dir`.
pub fn models_dir_disk(models_dir: Option<&str>) -> Option<(f64, f64)> {
    let path = match models_dir.filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => default_models_dir(),
    };
    disk_for_path(&path)
}

fn disk_for_path(path: &Path) -> Option<(f64, f64)> {
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(&Path, u64, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if path.starts_with(mount) {
            let better = match best {
                None => true,
                Some((prev, _, _)) => mount.as_os_str().len() > prev.as_os_str().len(),
            };
            if better {
                best = Some((mount, disk.total_space(), disk.available_space()));
            }
        }
    }
    best.filter(|(_, total, _)| *total > 0)
        .map(|(_, total, avail)| (bytes_to_gib(total), bytes_to_gib(avail)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_dir_is_non_empty() {
        assert!(!default_models_dir().as_os_str().is_empty());
    }
}
