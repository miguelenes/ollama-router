//! Marker file so converge is a state machine, not `if command -v`.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const STATE_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConvergeState {
    pub schema: u32,
    pub ollama_installed: bool,
    pub ollama_version: Option<String>,
    pub unit_written: bool,
    pub last_converge: Option<String>,
    pub listen_mode: Option<String>,
    pub bind: Option<String>,
}

impl ConvergeState {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self {
                schema: STATE_SCHEMA,
                ..Self::default()
            };
        };
        serde_json::from_str(&raw).unwrap_or(Self {
            schema: STATE_SCHEMA,
            ..Self::default()
        })
    }

    pub fn store(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
