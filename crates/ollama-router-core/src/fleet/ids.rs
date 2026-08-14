//! Stable identity newtypes for fleet membership and Verda instances.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn parse_nonempty<'a>(raw: &'a str, kind: &str) -> Result<&'a str, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{kind} must be non-empty"));
    }
    Ok(trimmed)
}

/// Fleet node identity. `Clone` is an `Arc` bump.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeId(Arc<str>);

impl NodeId {
    /// Parse a non-empty identifier.
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, String> {
        parse_nonempty(raw.as_ref(), "node id").map(|s| Self(Arc::from(s)))
    }

    /// Known-good non-empty `'static` identifier.
    ///
    /// Empty input is a programming error. Debug builds assert; release maps to `"_"`.
    pub fn from_static(raw: &'static str) -> Self {
        debug_assert!(
            !raw.is_empty(),
            "NodeId::from_static requires a non-empty literal"
        );
        if raw.is_empty() {
            return Self(Arc::from("_"));
        }
        Self(Arc::from(raw))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for NodeId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for NodeId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for NodeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

macro_rules! nonempty_id {
    ($name:ident, $label:expr) => {
        #[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Parse a non-empty identifier.
            pub fn parse(raw: impl AsRef<str>) -> Result<Self, String> {
                parse_nonempty(raw.as_ref(), $label).map(|s| Self(s.to_string()))
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

nonempty_id!(RouterId, "router id");
nonempty_id!(VerdaInstanceId, "verda instance id");

impl RouterId {
    /// Fallback identity when `router_id_env` and hostname are unavailable.
    pub fn fallback() -> Self {
        Self("ollama-router".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_parse_rejects_empty() {
        assert!(NodeId::parse("").is_err());
        assert!(NodeId::parse("  ").is_err());
    }

    #[test]
    fn node_id_from_static_and_clone_share_content() {
        let a = NodeId::from_static("gpu-1");
        let b = a.clone();
        assert_eq!(a.as_str(), "gpu-1");
        assert_eq!(a, b);
        assert_eq!(NodeId::parse("gpu-1").expect("parse"), a);
    }
}
