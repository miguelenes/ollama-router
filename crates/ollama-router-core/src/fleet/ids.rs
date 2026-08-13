//! Stable identity newtypes for fleet membership and Verda instances.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

fn parse_nonempty(raw: &str, kind: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{kind} must be non-empty"));
    }
    Ok(trimmed.to_string())
}

macro_rules! nonempty_id {
    ($name:ident, $label:expr) => {
        #[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse a non-empty identifier.
            pub fn parse(raw: impl AsRef<str>) -> Result<Self, String> {
                parse_nonempty(raw.as_ref(), $label).map(Self)
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

nonempty_id!(NodeId, "node id");
nonempty_id!(RouterId, "router id");
nonempty_id!(VerdaInstanceId, "verda instance id");

impl RouterId {
    /// Fallback identity when `router_id_env` and hostname are unavailable.
    pub fn fallback() -> Self {
        Self("ollama-router".to_string())
    }
}
