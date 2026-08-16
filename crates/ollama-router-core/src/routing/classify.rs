//! Request-class inference from path + model name.
//!
//! Pure functions: no I/O.

use std::fmt;

use crate::config::PolicyConfig;

/// Substrings that mark an embedding model.
pub const DEFAULT_EMBED_MARKERS: &[&str] = &["embed", "e5-", "bge-", "arctic-embed"];

/// Untagged base names that route to [`RequestClass::Small`].
pub const DEFAULT_SMALL_MODEL_BASES: &[&str] = &["moondream", "minicpm-v"];

/// Routing request class (includes pull/generic, unlike YAML RAM-policy classes).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RequestClass {
    Embed,
    Small,
    Medium,
    Large,
    Pull,
    Generic,
}

impl RequestClass {
    /// Wire / debug-header value (`embed`, `small`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embed => "embed",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Pull => "pull",
            Self::Generic => "generic",
        }
    }

    /// Map to the 4-variant YAML RAM-policy class, if this class can appear there.
    pub fn as_policy_class(self) -> Option<crate::config::RequestClass> {
        match self {
            Self::Embed => Some(crate::config::RequestClass::Embed),
            Self::Small => Some(crate::config::RequestClass::Small),
            Self::Medium => Some(crate::config::RequestClass::Medium),
            Self::Large => Some(crate::config::RequestClass::Large),
            Self::Pull | Self::Generic => None,
        }
    }
}

impl fmt::Display for RequestClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameter count in billions from a `:Nb` tag suffix.
///
/// `llama3.2:3b` → 3.0, `qwen3:30b-a3b` → 30.0 (total params, not active).
pub fn parse_model_size_b(model: &str) -> Option<f64> {
    let lowered = model.trim().to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let rest = &lowered[i + 1..];
            let r = rest.as_bytes();
            let mut num_end = 0;
            while num_end < r.len() && r[num_end].is_ascii_digit() {
                num_end += 1;
            }
            if num_end > 0 && num_end < r.len() && r[num_end] == b'.' {
                let mut frac = num_end + 1;
                let frac_start = frac;
                while frac < r.len() && r[frac].is_ascii_digit() {
                    frac += 1;
                }
                if frac > frac_start {
                    num_end = frac;
                }
            }
            if num_end > 0 && num_end < r.len() && r[num_end] == b'b' {
                let after = num_end + 1;
                if after == r.len() || matches!(r[after], b'-' | b'_' | b'.') {
                    return rest[..num_end].parse().ok();
                }
            }
        }
        i += 1;
    }
    None
}

/// Parameter count in billions from Ollama `details.parameter_size` (`1B`, `1.2B`, `8.0B`).
pub fn parse_parameter_size_b(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut num_end = 0;
    while num_end < bytes.len() && bytes[num_end].is_ascii_digit() {
        num_end += 1;
    }
    if num_end > 0 && num_end < bytes.len() && bytes[num_end] == b'.' {
        let mut frac = num_end + 1;
        let frac_start = frac;
        while frac < bytes.len() && bytes[frac].is_ascii_digit() {
            frac += 1;
        }
        if frac > frac_start {
            num_end = frac;
        }
    }
    if num_end == 0 || num_end >= bytes.len() || bytes[num_end] != b'b' {
        return None;
    }
    if num_end + 1 != bytes.len() {
        return None;
    }
    lowered[..num_end].parse().ok()
}

/// Size hint from a tags-probe `details` object (`parameter_size` only).
pub fn size_hint_from_tag_details(details: Option<&serde_json::Value>) -> Option<f64> {
    details
        .and_then(|d| d.get("parameter_size"))
        .and_then(|v| v.as_str())
        .and_then(parse_parameter_size_b)
}

fn class_from_size_b(size: f64, policy: &PolicyConfig) -> RequestClass {
    if size <= policy.small_max_b {
        RequestClass::Small
    } else if size <= policy.medium_max_b {
        RequestClass::Medium
    } else {
        RequestClass::Large
    }
}

/// Whether `model` looks like an embedding model.
pub fn looks_like_embedding(model: &str, markers: &[&str]) -> bool {
    let lowered = model.trim().to_ascii_lowercase();
    markers.iter().any(|marker| lowered.contains(marker))
}

/// Known-small base name (ignores the tag).
pub fn is_known_small_base(model: &str, bases: &[&str]) -> bool {
    let lowered = model.trim().to_ascii_lowercase();
    let base = lowered.split_once(':').map(|(b, _)| b).unwrap_or(&lowered);
    bases.contains(&base)
}

/// Native and OpenAI inference paths. The proxy additionally requires `POST`
/// before `inflight_inc` / reservation (idle activity).
pub fn is_inference_path(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/'),
        "/api/generate"
            | "/api/chat"
            | "/api/embed"
            | "/api/embeddings"
            | "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/embeddings"
    )
}

/// Class implied by the endpoint alone (`None` = needs the model name).
pub fn classify_path(path: &str) -> Option<RequestClass> {
    let p = path.trim_end_matches('/');
    match p {
        "/api/embed" | "/api/embeddings" | "/v1/embeddings" => Some(RequestClass::Embed),
        "/api/pull" => Some(RequestClass::Pull),
        // Metadata lookup: never size-class as LARGE / generate-timeout.
        "/api/show" => Some(RequestClass::Generic),
        _ => None,
    }
}

/// Full request-class decision from path + model name + policy.
pub fn classify(path: &str, model: Option<&str>, policy: &PolicyConfig) -> RequestClass {
    classify_with_size_hint(path, model, policy, None)
}

/// [`classify`] with an optional catalog `parameter_size` hint (billions).
///
/// The hint is used only when the model name has no parseable `:Nb` suffix
/// (after embed markers and known-small bases).
pub fn classify_with_size_hint(
    path: &str,
    model: Option<&str>,
    policy: &PolicyConfig,
    size_hint_b: Option<f64>,
) -> RequestClass {
    classify_with_markers(path, model, policy, DEFAULT_EMBED_MARKERS, size_hint_b)
}

/// [`classify`] with injectable embedding markers.
pub fn classify_with_markers(
    path: &str,
    model: Option<&str>,
    policy: &PolicyConfig,
    embed_markers: &[&str],
    size_hint_b: Option<f64>,
) -> RequestClass {
    if let Some(path_class) = classify_path(path) {
        return path_class;
    }
    if let Some(model) = model {
        if looks_like_embedding(model, embed_markers) {
            return RequestClass::Embed;
        }
        if is_known_small_base(model, DEFAULT_SMALL_MODEL_BASES) {
            return RequestClass::Small;
        }
        if let Some(size) = parse_model_size_b(model) {
            return class_from_size_b(size, policy);
        }
        if let Some(size) = size_hint_b {
            return class_from_size_b(size, policy);
        }
        RequestClass::Medium
    } else {
        RequestClass::Generic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolicyConfig;

    fn policy() -> PolicyConfig {
        PolicyConfig {
            small_max_b: 4.0,
            medium_max_b: 9.0,
            medium_min_vram_gb: 8.0,
            ..PolicyConfig::default()
        }
    }

    #[test]
    fn parse_model_size_b_table() {
        let cases = [
            ("llama3.2:3b", Some(3.0)),
            ("llama3.2:1b", Some(1.0)),
            ("qwen3-embedding:8b", Some(8.0)),
            ("qwen3:30b-a3b", Some(30.0)),
            ("llama3.1:70b-instruct-q4_K_M", Some(70.0)),
            ("gpt-oss:20b", Some(20.0)),
            ("phi4:14b-qat", Some(14.0)),
            ("moondream", None),
            ("llama3.2", None),
            ("granite4:tiny", None),
        ];
        for (model, size) in cases {
            assert_eq!(parse_model_size_b(model), size, "{model}");
        }
    }

    #[test]
    fn embedding_markers() {
        for model in [
            "qwen3-embedding:8b",
            "qwen3-embedding:0.6b",
            "nomic-embed-text",
            "bge-m3",
            "e5-large",
            "snowflake-arctic-embed",
        ] {
            assert!(
                looks_like_embedding(model, DEFAULT_EMBED_MARKERS),
                "{model}"
            );
        }
        assert!(!looks_like_embedding("llama3.2:3b", DEFAULT_EMBED_MARKERS));
        assert!(!looks_like_embedding("moondream", DEFAULT_EMBED_MARKERS));
    }

    #[test]
    fn parse_parameter_size_b_table() {
        let cases = [
            ("1B", Some(1.0)),
            ("1b", Some(1.0)),
            ("1.2B", Some(1.2)),
            ("8.0B", Some(8.0)),
            ("70B", Some(70.0)),
            ("", None),
            ("B", None),
            ("1.2", None),
            ("1.2BB", None),
            ("13B-instruct", None),
        ];
        for (raw, size) in cases {
            assert_eq!(parse_parameter_size_b(raw), size, "{raw}");
        }
    }

    #[test]
    fn size_hint_from_details() {
        let details = serde_json::json!({"family": "minicpm", "parameter_size": "1B"});
        assert_eq!(size_hint_from_tag_details(Some(&details)), Some(1.0));
        assert_eq!(size_hint_from_tag_details(None), None);
        assert_eq!(
            size_hint_from_tag_details(Some(&serde_json::json!({"family": "llama"}))),
            None
        );
    }

    #[test]
    fn classify_uses_parameter_size_hint_when_no_nb() {
        let p = policy();
        assert_eq!(
            classify_with_size_hint("/api/chat", Some("minicpm-v4.6:latest"), &p, Some(1.0)),
            RequestClass::Small
        );
        assert_eq!(
            classify_with_size_hint("/api/chat", Some("custom-modelfile:latest"), &p, None),
            RequestClass::Medium
        );
        assert_eq!(
            classify_with_size_hint("/api/chat", Some("llama3.1:70b"), &p, Some(1.0)),
            RequestClass::Large
        );
        assert_eq!(
            classify("/api/chat", Some("qwen3:30b-a3b"), &p),
            RequestClass::Large
        );
        assert_eq!(
            classify("/api/show", Some("llama3.1:70b"), &p),
            RequestClass::Generic
        );
        assert_eq!(
            classify("/v1/completions", Some("llama3.2:3b"), &p),
            RequestClass::Small
        );
    }

    #[test]
    fn classify_table() {
        let p = policy();
        let cases: &[(&str, Option<&str>, RequestClass)] = &[
            (
                "/api/embed",
                Some("qwen3-embedding:8b"),
                RequestClass::Embed,
            ),
            ("/api/embeddings", None, RequestClass::Embed),
            ("/api/pull", Some("llama3.2:3b"), RequestClass::Pull),
            (
                "/api/generate",
                Some("qwen3-embedding:8b"),
                RequestClass::Embed,
            ),
            ("/api/chat", Some("moondream"), RequestClass::Small),
            ("/api/chat", Some("llama3.2:1b"), RequestClass::Small),
            ("/api/chat", Some("llama3.2:3b"), RequestClass::Small),
            ("/api/chat", Some("llama3.1:8b"), RequestClass::Medium),
            (
                "/api/generate",
                Some("qwen2.5:7b-instruct"),
                RequestClass::Medium,
            ),
            ("/api/chat", Some("llama3.1:70b"), RequestClass::Large),
            ("/api/chat", Some("qwen3:32b"), RequestClass::Large),
            ("/api/chat", Some("llama3.2"), RequestClass::Medium),
            ("/api/tags", None, RequestClass::Generic),
            ("/api/show", None, RequestClass::Generic),
            ("/api/show", Some("llama3.1:70b"), RequestClass::Generic),
            ("/v1/embeddings", Some("all-minilm"), RequestClass::Embed),
            ("/v1/embeddings", None, RequestClass::Embed),
            (
                "/v1/chat/completions",
                Some("llama3.1:70b"),
                RequestClass::Large,
            ),
            (
                "/v1/chat/completions",
                Some("llama3.2:3b"),
                RequestClass::Small,
            ),
            ("/v1/completions", Some("llama3.1:8b"), RequestClass::Medium),
            (
                "/v1/chat/completions",
                Some("qwen3-embedding:8b"),
                RequestClass::Embed,
            ),
        ];
        for (path, model, expected) in cases {
            assert_eq!(classify(path, *model, &p), *expected, "{path} {model:?}");
        }
    }

    #[test]
    fn small_base_names_override_size() {
        let p = policy();
        assert_eq!(
            classify("/api/chat", Some("moondream"), &p),
            RequestClass::Small
        );
        assert_eq!(
            classify("/api/chat", Some("minicpm-v"), &p),
            RequestClass::Small
        );
    }

    #[test]
    fn inference_path_allowlist() {
        for path in [
            "/api/generate",
            "/api/chat",
            "/api/embed",
            "/api/embeddings",
            "/v1/chat/completions",
            "/v1/completions",
            "/v1/embeddings",
            "/v1/chat/completions/",
        ] {
            assert!(is_inference_path(path), "{path}");
        }
        for path in [
            "/api/show",
            "/api/ps",
            "/api/tags",
            "/v1/models",
            "/v1/images/generations",
            "/api/push",
        ] {
            assert!(!is_inference_path(path), "{path}");
        }
    }

    #[test]
    fn custom_thresholds() {
        let p = PolicyConfig {
            small_max_b: 2.0,
            medium_max_b: 7.0,
            ..PolicyConfig::default()
        };
        assert_eq!(
            classify("/api/chat", Some("llama3.2:3b"), &p),
            RequestClass::Medium
        );
        assert_eq!(
            classify("/api/chat", Some("qwen2.5:7b"), &p),
            RequestClass::Medium
        );
        assert_eq!(
            classify("/api/chat", Some("llama3.1:8b"), &p),
            RequestClass::Large
        );
    }
}
