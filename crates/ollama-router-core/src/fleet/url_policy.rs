//! Routing URL reachability: loopback and RFC1918 are ok; public IPv4,
//! CGNAT (`100.64/10`), and public-share hostnames are blocked.
//!
//! `100.64.0.0/10` is shared CGNAT, not an overlay we route on. Do not
//! convert those addresses into Ollama URLs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const DEFAULT_PUBLIC_SHARE_SUFFIX: &str = ".zrok.io";

fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_string)
}

/// True when `url`'s host is loopback (`127.0.0.1`, `::1`, `localhost`).
pub fn url_host_is_loopback(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    host_is_loopback(&host)
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(IpAddr::V4(addr)) = host.parse::<IpAddr>() {
        return addr.is_loopback();
    }
    if let Ok(addr) = host.parse::<Ipv4Addr>() {
        return addr.is_loopback();
    }
    if let Ok(addr) = host.parse::<Ipv6Addr>() {
        return addr.is_loopback();
    }
    false
}

/// True when `url`'s host is RFC1918 private IPv4.
pub fn url_host_is_rfc1918(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    host.parse::<Ipv4Addr>().is_ok_and(|addr| addr.is_private())
}

/// True when `url`'s host is any IP address (v4 or v6).
pub fn url_host_is_ip(url: &str) -> bool {
    url_host(url)
        .map(|host| host.parse::<IpAddr>().is_ok())
        .unwrap_or(false)
}

fn ipv4_is_cgnat(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 100 && (octets[1] & 0xC0) == 0x40
}

/// True when `url`'s host is globally-routable IPv4 **or** CGNAT (`100.64/10`).
///
/// Hostnames, loopback, and RFC1918 return false so `127.0.0.1` httpmock,
/// `mock-cpu` compose, and LAN `10.x` stay probeable. `http://8.8.8.8:11434`
/// and `http://100.64.0.1:11434` are `public_url_blocked`.
pub fn url_host_is_public_ipv4(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    let Ok(addr) = host.parse::<Ipv4Addr>() else {
        return false;
    };
    if ipv4_is_cgnat(addr) {
        return true;
    }
    ipv4_is_global(addr)
}

/// Python `IPv4Address.is_global` minus CGNAT (handled separately as blocked).
fn ipv4_is_global(addr: Ipv4Addr) -> bool {
    if addr.is_unspecified()
        || addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_multicast()
    {
        return false;
    }
    // 240.0.0.0/4 reserved
    if addr.octets()[0] >= 240 {
        return false;
    }
    // IETF protocol assignments 192.0.0.0/24 except 192.0.0.9/10
    let oct = addr.octets();
    if oct[0] == 192 && oct[1] == 0 && oct[2] == 0 {
        return oct[3] == 9 || oct[3] == 10;
    }
    // Documentation 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
    if oct[0] == 192 && oct[1] == 0 && oct[2] == 2 {
        return false;
    }
    if oct[0] == 198 && oct[1] == 51 && oct[2] == 100 {
        return false;
    }
    if oct[0] == 203 && oct[1] == 0 && oct[2] == 113 {
        return false;
    }
    // Benchmarking 198.18.0.0/15
    if oct[0] == 198 && (oct[1] == 18 || oct[1] == 19) {
        return false;
    }
    true
}

fn normalize_suffix(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('.').to_ascii_lowercase();
    format!(".{trimmed}")
}

/// `.zrok.io` plus operator extras, de-duplicated.
pub fn effective_public_share_suffixes(extra: &[String]) -> Vec<String> {
    let mut out = vec![DEFAULT_PUBLIC_SHARE_SUFFIX.to_string()];
    for item in extra {
        let normalized = normalize_suffix(item);
        if normalized == "." {
            continue;
        }
        if !out.iter().any(|existing| existing == &normalized) {
            out.push(normalized);
        }
    }
    out
}

fn host_matches_suffix(host: &str, suffix: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let suffix = normalize_suffix(suffix);
    if suffix.len() <= 1 {
        return false;
    }
    let bare = suffix.trim_start_matches('.');
    host == bare || host.ends_with(&suffix)
}

fn host_is_public_share(host: &str, extra_suffixes: &[String]) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    if host.is_empty() || host_is_loopback(host) {
        return false;
    }
    effective_public_share_suffixes(extra_suffixes)
        .iter()
        .any(|suffix| host_matches_suffix(host, suffix))
}

/// True when `url`'s host is a known public-share suffix (`*.zrok.io` + extras).
pub fn url_host_is_public_share(url: &str, extra_suffixes: &[String]) -> bool {
    url_host(url)
        .map(|host| host_is_public_share(&host, extra_suffixes))
        .unwrap_or(false)
}

/// Share unique-name that is actually a public URL or `*.zrok.io` hostname.
pub fn share_id_looks_public(share_id: &str, extra_suffixes: &[String]) -> bool {
    let trimmed = share_id.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("://") {
        return url_host_is_public_share(trimmed, extra_suffixes)
            || url_host_is_public_ipv4(trimmed);
    }
    host_is_public_share(trimmed, extra_suffixes)
}

/// Allowlisted health/admin reason when a routing URL must not be probed.
pub fn routing_url_blocked_reason(url: &str, extra_suffixes: &[String]) -> Option<&'static str> {
    if url_host_is_public_ipv4(url) || url_host_is_public_share(url, extra_suffixes) {
        Some("public_url_blocked")
    } else {
        None
    }
}

/// Loopback, RFC1918, or a hostname that is not a public-share frontend.
///
/// Public IPv4 (including CGNAT) and `*.zrok.io` (plus extras) are not overlay URLs.
pub fn url_is_safe_overlay(url: &str) -> bool {
    url_is_safe_overlay_with_suffixes(url, &[])
}

/// [`url_is_safe_overlay`] with operator extra public-share suffixes.
pub fn url_is_safe_overlay_with_suffixes(url: &str, extra_suffixes: &[String]) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    if url_host_is_public_ipv4(trimmed) {
        return false;
    }
    if url_host_is_public_share(trimmed, extra_suffixes) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ipv4_blocks_global_and_cgnat_not_private() {
        assert!(url_host_is_public_ipv4("http://8.8.8.8:11434"));
        assert!(url_host_is_public_ipv4("http://1.1.1.1:11434"));
        assert!(url_host_is_public_ipv4("http://100.64.0.1:11434"));
        assert!(url_host_is_public_ipv4("http://100.127.255.255:11434"));
        assert!(!url_host_is_public_ipv4("http://127.0.0.1:11434"));
        assert!(!url_host_is_public_ipv4("http://10.0.0.5:11434"));
        assert!(!url_host_is_public_ipv4("http://192.168.1.10:11434"));
        assert!(!url_host_is_public_ipv4("http://mock-cpu:11434"));
        assert!(!url_host_is_public_ipv4(
            "http://host.docker.internal:11434"
        ));
    }

    #[test]
    fn loopback_and_rfc1918_are_ok() {
        assert!(url_host_is_loopback("http://127.0.0.1:41999"));
        assert!(url_host_is_loopback("http://localhost:11434"));
        assert!(url_host_is_rfc1918("http://10.0.0.5:11434"));
        assert!(url_host_is_rfc1918("http://192.168.1.10:11434"));
        assert!(!url_host_is_loopback("http://10.0.0.5:11434"));
        assert!(!url_host_is_loopback("http://mock-cpu:11434"));
        assert!(!url_host_is_rfc1918("http://100.64.0.1:11434"));
        assert!(url_is_safe_overlay("http://127.0.0.1:41990"));
        assert!(url_is_safe_overlay("http://10.0.0.5:11434"));
        assert!(url_is_safe_overlay("http://mock-cpu:11434"));
        assert!(!url_is_safe_overlay("http://100.64.0.1:11434"));
        assert!(!url_is_safe_overlay("http://8.8.8.8:11434"));
        assert!(!url_is_safe_overlay("https://abc.share.zrok.io"));
    }

    #[test]
    fn url_host_is_ip_classifies_addresses() {
        assert!(url_host_is_ip("http://8.8.8.8:11434"));
        assert!(url_host_is_ip("http://127.0.0.1:11434"));
        assert!(!url_host_is_ip("http://host.docker.internal:11434"));
    }

    #[test]
    fn zrok_io_public_share_always_blocked() {
        let extras = Vec::<String>::new();
        assert!(url_host_is_public_share(
            "https://abc.share.zrok.io",
            &extras
        ));
        assert!(url_host_is_public_share("http://foo.zrok.io:443", &extras));
        assert!(share_id_looks_public("https://abc.zrok.io", &extras));
        assert!(share_id_looks_public("abc.share.zrok.io", &extras));
        assert!(!share_id_looks_public("d6zu50wi2cm8", &extras));
        assert!(!url_host_is_public_share("http://127.0.0.1:9191", &extras));
        assert!(!url_host_is_public_share("http://mock-cpu:11434", &extras));
    }

    #[test]
    fn extra_suffixes_are_honored() {
        let extras = vec![".example.dev".into()];
        assert!(url_host_is_public_share("https://n1.example.dev", &extras));
        assert!(share_id_looks_public("n1.example.dev", &extras));
        assert!(!share_id_looks_public("n1.other.dev", &extras));
        assert!(!url_is_safe_overlay_with_suffixes(
            "https://n1.example.dev",
            &extras
        ));
    }

    #[test]
    fn blocked_reason_unifies_ipv4_hostname_and_cgnat() {
        let extras = Vec::<String>::new();
        assert_eq!(
            routing_url_blocked_reason("http://8.8.8.8:11434", &extras),
            Some("public_url_blocked")
        );
        assert_eq!(
            routing_url_blocked_reason("http://100.64.0.1:11434", &extras),
            Some("public_url_blocked")
        );
        assert_eq!(
            routing_url_blocked_reason("https://x.zrok.io", &extras),
            Some("public_url_blocked")
        );
        assert_eq!(
            routing_url_blocked_reason("http://127.0.0.1:11434", &extras),
            None
        );
        assert_eq!(
            routing_url_blocked_reason("http://10.0.0.5:11434", &extras),
            None
        );
    }
}
