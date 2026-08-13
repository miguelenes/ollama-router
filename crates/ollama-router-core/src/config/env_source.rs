//! Environment lookup used by config loading (injectable for tests).

/// Source of process environment variables.
pub trait EnvSource {
    /// Return a single variable, if set.
    fn var(&self, key: &str) -> Option<String>;
    /// Snapshot of all variables.
    fn vars(&self) -> Vec<(String, String)>;
}

/// Live process environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEnv;

impl EnvSource for OsEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn vars(&self) -> Vec<(String, String)> {
        std::env::vars().collect()
    }
}

impl EnvSource for std::collections::HashMap<String, String> {
    fn var(&self, key: &str) -> Option<String> {
        self.get(key).cloned()
    }

    fn vars(&self) -> Vec<(String, String)> {
        self.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}
