//! Routing URL reachability: loopback, RFC1918, link-local, and ULA are ok;
//! globally-routable IPs, CGNAT (`100.64/10`), unspecified (`0.0.0.0` / `::`),
//! and public-share hostnames are blocked.
//!
//! `100.64.0.0/10` is shared CGNAT, not an overlay we route on. Do not
//! convert those addresses into Ollama URLs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const DEFAULT_PUBLIC_SHARE_SUFFIX: &str = ".zrok.io";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostClass {
    Loopback,
    Private,
    LinkLocal,
    UniqueLocal,
    Hostname,
    BlockedPublic,
}

fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_string)
}

fn strip_brackets(host: &str) -> &str {
    host.trim().trim_matches(['[', ']'])
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    let host = strip_brackets(host);
    host.parse::<IpAddr>()
        .ok()
        .or_else(|| host.parse::<Ipv4Addr>().ok().map(IpAddr::V4))
        .or_else(|| host.parse::<Ipv6Addr>().ok().map(IpAddr::V6))
}

/// Unmap IPv4-mapped and IPv4-compatible v6 **after** treating `::1` as loopback.
///
/// `Ipv6Addr::to_ipv4()` would turn `::1` into `0.0.0.1`, which is not loopback.
fn canonical_ip(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return IpAddr::V6(v6);
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            if let Some(v4) = v6.to_ipv4() {
                return IpAddr::V4(v4);
            }
            IpAddr::V6(v6)
        }
    }
}

fn classify_ip(addr: IpAddr) -> HostClass {
    match canonical_ip(addr) {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                HostClass::Loopback
            } else if v4.is_private() {
                HostClass::Private
            } else if v4.is_link_local() {
                HostClass::LinkLocal
            } else {
                HostClass::BlockedPublic
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                HostClass::Loopback
            } else if v6.is_unique_local() {
                HostClass::UniqueLocal
            } else if v6.is_unicast_link_local() {
                HostClass::LinkLocal
            } else {
                HostClass::BlockedPublic
            }
        }
    }
}

fn classify_host(host: &str) -> HostClass {
    let host = strip_brackets(host);
    if host.eq_ignore_ascii_case("localhost") {
        return HostClass::Loopback;
    }
    match parse_ip(host) {
        Some(addr) => classify_ip(addr),
        None => HostClass::Hostname,
    }
}

fn classify_url_host(url: &str) -> Option<HostClass> {
    url_host(url).map(|host| classify_host(&host))
}

/// True when `url`'s host is loopback (`127.0.0.1`, `::1`, `localhost`).
pub fn url_host_is_loopback(url: &str) -> bool {
    classify_url_host(url) == Some(HostClass::Loopback)
}

fn host_is_loopback(host: &str) -> bool {
    classify_host(host) == HostClass::Loopback
}

/// True when `url`'s host is RFC1918 private IPv4 (including IPv4-mapped v6).
pub fn url_host_is_rfc1918(url: &str) -> bool {
    classify_url_host(url) == Some(HostClass::Private)
}

/// True when `url`'s host is any IP address (v4 or v6).
pub fn url_host_is_ip(url: &str) -> bool {
    url_host(url)
        .map(|host| parse_ip(&host).is_some())
        .unwrap_or(false)
}

/// True when `url`'s host is a blocked public IP: globally routable v4/v6,
/// CGNAT (`100.64/10`), unspecified (`0.0.0.0` / `::`), multicast, or
/// documentation ranges. Loopback, RFC1918, link-local, ULA, and hostnames
/// return false.
pub fn url_host_is_public_ip(url: &str) -> bool {
    classify_url_host(url) == Some(HostClass::BlockedPublic)
}

/// Alias of [`url_host_is_public_ip`] (historical name; IPv6 is included).
pub fn url_host_is_public_ipv4(url: &str) -> bool {
    url_host_is_public_ip(url)
}

#[cfg(test)]
fn ipv4_is_cgnat(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 100 && (octets[1] & 0xC0) == 0x40
}

#[cfg(test)]
fn ipv4_is_global_for_proptest(addr: Ipv4Addr) -> bool {
    if addr.is_unspecified()
        || addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_broadcast()
        || addr.is_multicast()
    {
        return false;
    }
    if addr.octets()[0] >= 240 {
        return false;
    }
    let oct = addr.octets();
    if oct[0] == 192 && oct[1] == 0 && oct[2] == 0 {
        return oct[3] == 9 || oct[3] == 10;
    }
    if oct[0] == 192 && oct[1] == 0 && oct[2] == 2 {
        return false;
    }
    if oct[0] == 198 && oct[1] == 51 && oct[2] == 100 {
        return false;
    }
    if oct[0] == 203 && oct[1] == 0 && oct[2] == 113 {
        return false;
    }
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
    let host = strip_brackets(host);
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
        return url_host_is_public_share(trimmed, extra_suffixes) || url_host_is_public_ip(trimmed);
    }
    classify_host(trimmed) == HostClass::BlockedPublic
        || host_is_public_share(trimmed, extra_suffixes)
}

/// Allowlisted health/admin reason when a routing URL must not be probed.
pub fn routing_url_blocked_reason(url: &str, extra_suffixes: &[String]) -> Option<&'static str> {
    if url_host_is_public_ip(url) || url_host_is_public_share(url, extra_suffixes) {
        Some("public_url_blocked")
    } else {
        None
    }
}

/// Loopback, RFC1918, link-local, ULA, or a hostname that is not a public-share frontend.
///
/// Public IPs (including CGNAT and unspecified) and `*.zrok.io` (plus extras)
/// are not overlay URLs.
pub fn url_is_safe_overlay(url: &str) -> bool {
    url_is_safe_overlay_with_suffixes(url, &[])
}

/// [`url_is_safe_overlay`] with operator extra public-share suffixes.
pub fn url_is_safe_overlay_with_suffixes(url: &str, extra_suffixes: &[String]) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return false;
    }
    if url_host_is_public_ip(trimmed) {
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
        assert!(url_host_is_public_ip("http://8.8.8.8:11434"));
        assert!(url_host_is_public_ip("http://1.1.1.1:11434"));
        assert!(url_host_is_public_ip("http://100.64.0.1:11434"));
        assert!(url_host_is_public_ip("http://100.127.255.255:11434"));
        assert!(!url_host_is_public_ip("http://127.0.0.1:11434"));
        assert!(!url_host_is_public_ip("http://10.0.0.5:11434"));
        assert!(!url_host_is_public_ip("http://192.168.1.10:11434"));
        assert!(!url_host_is_public_ip("http://mock-cpu:11434"));
        assert!(!url_host_is_public_ip("http://host.docker.internal:11434"));
        assert!(url_host_is_public_ipv4("http://8.8.8.8:11434"));
    }

    #[test]
    fn public_ipv6_and_mapped_are_blocked() {
        assert!(url_host_is_public_ip("http://[2606:4700:4700::1111]:11434"));
        assert!(url_host_is_public_ip("http://[::ffff:8.8.8.8]:11434"));
        assert!(url_host_is_public_ip("http://[::ffff:100.64.0.1]:11434"));
        assert!(url_host_is_public_ip("http://[::8.8.8.8]:11434"));
        assert!(url_is_safe_overlay("http://[::1]:11434"));
        assert!(url_is_safe_overlay("http://[::ffff:127.0.0.1]:11434"));
        assert!(url_is_safe_overlay("http://[::ffff:10.0.0.5]:11434"));
        assert!(url_is_safe_overlay("http://[fd12:3456::1]:11434"));
        assert!(url_is_safe_overlay("http://[fe80::1]:11434"));
        assert!(!url_is_safe_overlay("http://[2606:4700:4700::1111]:11434"));
        assert!(url_host_is_loopback("http://[::1]:11434"));
        assert!(url_host_is_loopback("http://[::ffff:127.0.0.1]:11434"));
        assert!(url_host_is_rfc1918("http://[::ffff:10.0.0.5]:11434"));
        assert!(share_id_looks_public(
            "http://[2606:4700:4700::1111]:11434",
            &[]
        ));
    }

    #[test]
    fn unspecified_addresses_are_blocked() {
        assert!(url_host_is_public_ip("http://0.0.0.0:11434"));
        assert!(url_host_is_public_ip("http://[::]:11434"));
        assert!(!url_is_safe_overlay("http://0.0.0.0:11434"));
        assert!(!url_is_safe_overlay("http://[::]:11434"));
        assert_eq!(
            routing_url_blocked_reason("http://0.0.0.0:11434", &[]),
            Some("public_url_blocked")
        );
        assert_eq!(
            routing_url_blocked_reason("http://[::]:11434", &[]),
            Some("public_url_blocked")
        );
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
        assert!(url_is_safe_overlay("http://169.254.1.1:11434"));
        assert!(url_is_safe_overlay("http://mock-cpu:11434"));
        assert!(!url_is_safe_overlay("http://100.64.0.1:11434"));
        assert!(!url_is_safe_overlay("http://8.8.8.8:11434"));
        assert!(!url_is_safe_overlay("https://abc.share.zrok.io"));
    }

    #[test]
    fn url_host_is_ip_classifies_addresses() {
        assert!(url_host_is_ip("http://8.8.8.8:11434"));
        assert!(url_host_is_ip("http://127.0.0.1:11434"));
        assert!(url_host_is_ip("http://[::1]:11434"));
        assert!(url_host_is_ip("http://[2606:4700:4700::1111]:11434"));
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
    fn runpod_proxy_suffix_blocks_public_proxy_hostname() {
        let extras = vec![".proxy.runpod.net".into()];
        assert_eq!(
            routing_url_blocked_reason("https://something.proxy.runpod.net", &extras),
            Some("public_url_blocked")
        );
        assert!(!url_is_safe_overlay_with_suffixes(
            "https://something.proxy.runpod.net",
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
            routing_url_blocked_reason("http://[2606:4700:4700::1111]:11434", &extras),
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
        assert_eq!(
            routing_url_blocked_reason("http://[::1]:11434", &extras),
            None
        );
    }

    #[test]
    fn documentation_ipv6_is_blocked() {
        assert!(url_host_is_public_ip("http://[2001:db8::1]:11434"));
        assert!(!url_is_safe_overlay("http://[2001:db8::1]:11434"));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn v4_allowlist_is_loopback_private_or_link_local(octets in any::<[u8; 4]>()) {
            let addr = Ipv4Addr::from(octets);
            let url = format!("http://{addr}:11434");
            let allowed = addr.is_loopback() || addr.is_private() || addr.is_link_local();
            if allowed {
                prop_assert!(!url_host_is_public_ip(&url), "{url}");
                prop_assert!(url_is_safe_overlay(&url), "{url}");
            } else {
                prop_assert!(url_host_is_public_ip(&url), "{url}");
                prop_assert!(!url_is_safe_overlay(&url), "{url}");
            }
            if ipv4_is_global_for_proptest(addr) || ipv4_is_cgnat(addr) {
                prop_assert!(url_host_is_public_ip(&url), "{url}");
            }
        }

        #[test]
        fn v6_allowlist_after_canonical_unmap(segments in any::<[u16; 8]>()) {
            let addr = Ipv6Addr::from(segments);
            let url = format!("http://[{addr}]:11434");
            match classify_ip(IpAddr::V6(addr)) {
                HostClass::Loopback | HostClass::Private | HostClass::LinkLocal | HostClass::UniqueLocal => {
                    prop_assert!(!url_host_is_public_ip(&url), "{url}");
                    prop_assert!(url_is_safe_overlay(&url), "{url}");
                }
                HostClass::BlockedPublic => {
                    prop_assert!(url_host_is_public_ip(&url), "{url}");
                    prop_assert!(!url_is_safe_overlay(&url), "{url}");
                }
                HostClass::Hostname => prop_assert!(false, "IP classified as hostname: {url}"),
            }
        }
    }
}
