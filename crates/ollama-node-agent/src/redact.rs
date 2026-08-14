//! Redact enable tokens and share tokens. Never log the raw value.

pub fn redact_secret(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => "(empty)".into(),
        Some(key) => format!("*** (len={})", key.len()),
    }
}

/// Operator-facing token id: unique-name as-is when short, otherwise a prefix.
/// Never an enable token.
pub fn share_token_id(token: &str) -> String {
    let token = token.trim();
    const KEEP: usize = 8;
    if token.len() <= KEEP {
        token.to_string()
    } else {
        format!("{}…", &token[..KEEP])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_length_only() {
        assert_eq!(redact_secret(None), "(empty)");
        assert_eq!(redact_secret(Some("")), "(empty)");
        assert_eq!(redact_secret(Some("  ")), "(empty)");
        assert_eq!(redact_secret(Some("enable-abc123")), "*** (len=13)");
        assert_eq!(redact_secret(Some("zrok-enable-secret")), "*** (len=18)");
    }

    #[test]
    fn share_id_is_prefix_not_full_secret() {
        assert_eq!(share_token_id("abc"), "abc");
        assert_eq!(share_token_id("abcdefgh"), "abcdefgh");
        assert_eq!(share_token_id("abcdefghij"), "abcdefgh…");
    }
}
