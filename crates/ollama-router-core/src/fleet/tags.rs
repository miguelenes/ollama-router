//! Per-node `/api/tags` records and fleet catalog merge.

use std::collections::HashMap;

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::ids::NodeId;

/// Fields retained from one node's list entry (not routing identity).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TagRecord {
    pub digest: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
    pub details: Option<Value>,
    pub capabilities: Option<Vec<String>>,
}

/// Fields retained from one node's `/api/ps` entry (CLI-safe; ignore extras on parse).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PsRecord {
    pub digest: String,
    pub size: Option<u64>,
    pub size_vram: Option<u64>,
    pub details: Option<Value>,
    pub expires_at: Option<String>,
    pub context_length: Option<u64>,
}

/// One row of the aggregated `/api/ps` union (per healthy node × loaded model).
#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedPs {
    pub name: String,
    pub node: String,
    pub digest: String,
    pub size: Option<u64>,
    pub size_vram: Option<u64>,
    pub details: Option<Value>,
    pub expires_at: Option<String>,
    pub context_length: Option<u64>,
}

/// Build fleet-union ps rows from healthy nodes' probe records.
pub(crate) fn merge_ps<'a>(nodes: impl IntoIterator<Item = PsNode<'a>>) -> Vec<AggregatedPs> {
    let mut out = Vec::new();
    for node in nodes {
        for (name, record) in node.records {
            let digest = effective_digest(name, &record.digest);
            out.push(AggregatedPs {
                name: name.clone(),
                node: node.id.as_str().to_string(),
                digest,
                size: record.size,
                size_vram: record.size_vram,
                details: record.details.clone(),
                expires_at: record.expires_at.clone(),
                context_length: record.context_length,
            });
        }
    }
    out.sort_by(|a, b| (&a.name, &a.node).cmp(&(&b.name, &b.node)));
    out
}

pub(crate) struct PsNode<'a> {
    pub id: &'a NodeId,
    pub records: &'a HashMap<String, PsRecord>,
}

/// One row of the aggregated `/api/tags` union.
#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedTag {
    pub name: String,
    pub nodes: Vec<String>,
    pub digest: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
    pub details: Option<Value>,
    pub capabilities: Option<Vec<String>>,
}

impl AggregatedTag {
    /// Unix seconds for OpenAI `created`, or `0` when `modified_at` is missing/unparseable.
    pub fn created_unix(&self) -> i64 {
        self.modified_at
            .as_deref()
            .and_then(parse_modified_at_unix)
            .unwrap_or(0)
    }
}

/// SHA-256 hex of the normalized name (64 characters).
pub fn placeholder_digest(normalized_name: &str) -> String {
    hex_lower(Sha256::digest(normalized_name.as_bytes()).as_slice())
}

/// Probe digest when it is at least 12 characters; otherwise a stable placeholder.
pub fn effective_digest(normalized_name: &str, digest: &str) -> String {
    if digest.len() >= 12 {
        digest.to_string()
    } else {
        placeholder_digest(normalized_name)
    }
}

pub(crate) fn parse_modified_at_unix(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    OffsetDateTime::parse(raw, &Rfc3339)
        .ok()
        .or_else(|| {
            truncate_rfc3339_fraction(raw)
                .and_then(|trimmed| OffsetDateTime::parse(&trimmed, &Rfc3339).ok())
        })
        .map(|ts| ts.unix_timestamp())
}

/// Rfc3339 in `time` accepts at most nanoseconds (9 fractional digits).
fn truncate_rfc3339_fraction(raw: &str) -> Option<String> {
    let dot = raw.find('.')?;
    let after = &raw[dot + 1..];
    let frac_len = after.bytes().take_while(u8::is_ascii_digit).count();
    if frac_len <= 9 {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..=dot]);
    out.push_str(&after[..9]);
    out.push_str(&after[frac_len..]);
    Some(out)
}

struct MergeSlot {
    winner_node: NodeId,
    record: TagRecord,
    nodes: Vec<String>,
}

/// Union healthy holders by normalized name. `records` is keyed by normalized name.
pub(crate) fn merge_catalog<'a>(
    nodes: impl IntoIterator<Item = CatalogNode<'a>>,
) -> Vec<AggregatedTag> {
    let mut by_name: HashMap<String, MergeSlot> = HashMap::new();
    for node in nodes {
        for name in node.models {
            let record = node.records.get(name).cloned().unwrap_or_default();
            match by_name.get_mut(name) {
                None => {
                    by_name.insert(
                        name.clone(),
                        MergeSlot {
                            winner_node: node.id.clone(),
                            record,
                            nodes: vec![node.id.as_str().to_string()],
                        },
                    );
                }
                Some(slot) => {
                    slot.nodes.push(node.id.as_str().to_string());
                    if challenger_wins(&slot.record, &slot.winner_node, &record, node.id) {
                        slot.winner_node = node.id.clone();
                        slot.record = record;
                    }
                }
            }
        }
    }
    let mut out: Vec<AggregatedTag> = by_name
        .into_iter()
        .map(|(name, mut slot)| {
            slot.nodes.sort();
            let digest = effective_digest(&name, &slot.record.digest);
            AggregatedTag {
                name,
                nodes: slot.nodes,
                digest,
                size: slot.record.size,
                modified_at: slot.record.modified_at,
                details: slot.record.details,
                capabilities: slot.record.capabilities,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub(crate) struct CatalogNode<'a> {
    pub id: &'a NodeId,
    pub models: &'a std::collections::HashSet<String>,
    pub records: &'a HashMap<String, TagRecord>,
}

fn challenger_wins(
    current: &TagRecord,
    current_id: &NodeId,
    challenger: &TagRecord,
    challenger_id: &NodeId,
) -> bool {
    let current_ts = current
        .modified_at
        .as_deref()
        .and_then(parse_modified_at_unix);
    let challenger_ts = challenger
        .modified_at
        .as_deref()
        .and_then(parse_modified_at_unix);
    match (challenger_ts, current_ts) {
        (Some(c), Some(cur)) if c != cur => c > cur,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => challenger_id.as_str() < current_id.as_str(),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::registry::normalize_model;
    use std::collections::HashSet;

    fn nid(id: &str) -> NodeId {
        NodeId::parse(id).expect("id")
    }

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|n| normalize_model(n)).collect()
    }

    fn rec(digest: &str, modified_at: Option<&str>) -> TagRecord {
        TagRecord {
            digest: digest.to_string(),
            size: Some(1),
            modified_at: modified_at.map(str::to_string),
            details: None,
            capabilities: None,
        }
    }

    #[test]
    fn placeholder_digest_is_64_hex_chars() {
        let digest = placeholder_digest("llama3.2:1b");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(digest, placeholder_digest("llama3.2:1b"));
        assert_ne!(digest, placeholder_digest("llama3.2:3b"));
    }

    #[test]
    fn effective_digest_keeps_long_probe_digest() {
        let d = "55fc3abd386771e5b5d1bbcc732f3c3f4df6e9f9f08f1131f9cc27ba2d1eec5b";
        assert_eq!(effective_digest("moondream:latest", d), d);
    }

    #[test]
    fn effective_digest_replaces_short_or_empty() {
        assert_eq!(
            effective_digest("llama3.2:1b", ""),
            placeholder_digest("llama3.2:1b")
        );
        assert_eq!(
            effective_digest("llama3.2:1b", "abc"),
            placeholder_digest("llama3.2:1b")
        );
    }

    #[test]
    fn merge_picks_newest_modified_at() {
        let a = nid("a");
        let b = nid("b");
        let models = names(&["llama3.2:1b"]);
        let rec_a = HashMap::from([(
            "llama3.2:1b".into(),
            rec("aaaaaaaaaaaa", Some("2026-01-01T00:00:00Z")),
        )]);
        let rec_b = HashMap::from([(
            "llama3.2:1b".into(),
            rec("bbbbbbbbbbbb", Some("2026-08-01T00:00:00Z")),
        )]);
        let rows = merge_catalog([
            CatalogNode {
                id: &a,
                models: &models,
                records: &rec_a,
            },
            CatalogNode {
                id: &b,
                models: &models,
                records: &rec_b,
            },
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].digest, "bbbbbbbbbbbb");
        assert_eq!(rows[0].nodes, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn merge_tie_break_smaller_node_id() {
        let z = nid("z");
        let a = nid("a");
        let models = names(&["m"]);
        let recs = HashMap::from([("m".into(), rec("zzzzzzzzzzzz", None))]);
        let recs_a = HashMap::from([("m".into(), rec("aaaaaaaaaaaa", None))]);
        let rows = merge_catalog([
            CatalogNode {
                id: &z,
                models: &models,
                records: &recs,
            },
            CatalogNode {
                id: &a,
                models: &models,
                records: &recs_a,
            },
        ]);
        assert_eq!(rows[0].digest, "aaaaaaaaaaaa");
        assert_eq!(rows[0].nodes, vec!["a".to_string(), "z".to_string()]);
    }

    #[test]
    fn parse_ollama_modified_at_with_offset_and_nanos() {
        let ts = parse_modified_at_unix("2026-08-14T09:41:31.258693569+01:00");
        let expected = OffsetDateTime::parse("2026-08-14T08:41:31Z", &Rfc3339)
            .expect("rfc3339")
            .unix_timestamp();
        assert_eq!(ts, Some(expected));
    }

    #[test]
    fn parse_modified_at_truncates_overlong_fraction() {
        let ts = parse_modified_at_unix("2026-08-01T00:00:00.1234567890123Z");
        let expected = OffsetDateTime::parse("2026-08-01T00:00:00Z", &Rfc3339)
            .expect("rfc3339")
            .unix_timestamp();
        assert_eq!(ts, Some(expected));
    }
}
