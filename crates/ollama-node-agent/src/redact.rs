//! Redact Tailscale auth keys. Never log the raw value.

pub fn redact_authkey(key: Option<&str>) -> String {
    match key.map(str::trim).filter(|s| !s.is_empty()) {
        None => "(empty)".into(),
        Some(key) if key.starts_with("tskey-") => format!("tskey-*** (len={})", key.len()),
        Some(key) => format!("*** (len={})", key.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tskey_prefix() {
        assert_eq!(redact_authkey(None), "(empty)");
        assert_eq!(redact_authkey(Some("")), "(empty)");
        assert_eq!(redact_authkey(Some("tskey-abc123")), "tskey-*** (len=12)");
        assert_eq!(redact_authkey(Some("other")), "*** (len=5)");
    }
}
