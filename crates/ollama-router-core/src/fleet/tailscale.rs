//! Tailscale CGNAT helpers for Ollama routing URLs.
//!
//! Routing happy-path for provisioned hosts is Tailscale-only
//! (`http://100.x.y.z:11434`). Public/LAN IPs belong on SSH only.

use std::net::{IpAddr, Ipv4Addr};

/// True for Tailscale CGNAT (`100.64.0.0/10`).
pub fn is_tailscale_ipv4(ip: &str) -> bool {
    ip.trim()
        .parse::<Ipv4Addr>()
        .map(ipv4_is_tailscale)
        .unwrap_or(false)
}

fn ipv4_is_tailscale(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 100 && (octets[1] & 0xC0) == 0x40
}

/// Build `http://{ip}:11434` for a Tailscale IPv4.
pub fn ollama_url_for_tailscale_ip(ip: &str) -> Result<String, String> {
    let trimmed = ip.trim();
    if !is_tailscale_ipv4(trimmed) {
        return Err(format!("not a Tailscale IPv4 address: {trimmed:?}"));
    }
    Ok(format!("http://{trimmed}:11434"))
}

fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    parsed.host_str().map(str::to_string)
}

/// True when `url`'s host is a Tailscale CGNAT IPv4.
pub fn url_host_is_tailscale(url: &str) -> bool {
    url_host(url)
        .map(|host| is_tailscale_ipv4(&host))
        .unwrap_or(false)
}

/// True when `url`'s host is any IP address (v4 or v6).
pub fn url_host_is_ip(url: &str) -> bool {
    url_host(url)
        .map(|host| host.parse::<IpAddr>().is_ok())
        .unwrap_or(false)
}

/// True when `url`'s host is a non-Tailscale globally-routable IPv4.
///
/// Hostnames, loopback, RFC1918, and Tailscale CGNAT return false so
/// `127.0.0.1` httpmock / `mock-cpu` compose / `100.x` spots stay probeable.
/// `http://8.8.8.8:11434` is `public_url_blocked`.
pub fn url_host_is_public_ipv4(url: &str) -> bool {
    let Some(host) = url_host(url) else {
        return false;
    };
    let Ok(addr) = host.parse::<Ipv4Addr>() else {
        return false;
    };
    if ipv4_is_tailscale(addr) {
        return false;
    }
    ipv4_is_global(addr)
}

/// Python `IPv4Address.is_global`: not private/loopback/link-local/multicast/
/// unspecified/broadcast/reserved, and not CGNAT (handled separately).
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

/// Prefer a Tailscale routing URL from a fleet-state entry.
pub fn routing_url_from_fields(url: Option<&str>, tailscale_ip: Option<&str>) -> Option<String> {
    if let Some(url) = url {
        let trimmed = url.trim();
        if !trimmed.is_empty() && url_host_is_tailscale(trimmed) {
            return Some(trimmed.trim_end_matches('/').to_string());
        }
    }
    if let Some(ip) = tailscale_ip {
        if is_tailscale_ipv4(ip) {
            return ollama_url_for_tailscale_ip(ip).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_cgnat_window() {
        assert!(is_tailscale_ipv4("100.64.0.1"));
        assert!(is_tailscale_ipv4("100.127.255.255"));
        assert!(!is_tailscale_ipv4("100.63.255.255"));
        assert!(!is_tailscale_ipv4("8.8.8.8"));
        assert!(!is_tailscale_ipv4("host.docker.internal"));
    }

    #[test]
    fn url_host_classification() {
        assert!(url_host_is_tailscale("http://100.64.0.1:11434"));
        assert!(url_host_is_ip("http://8.8.8.8:11434"));
        assert!(!url_host_is_ip("http://host.docker.internal:11434"));
    }

    #[test]
    fn public_ipv4_blocks_global_not_private_or_cgnat() {
        assert!(url_host_is_public_ipv4("http://8.8.8.8:11434"));
        assert!(url_host_is_public_ipv4("http://1.1.1.1:11434"));
        assert!(!url_host_is_public_ipv4("http://127.0.0.1:11434"));
        assert!(!url_host_is_public_ipv4("http://10.0.0.5:11434"));
        assert!(!url_host_is_public_ipv4("http://192.168.1.10:11434"));
        assert!(!url_host_is_public_ipv4("http://100.64.0.1:11434"));
        assert!(!url_host_is_public_ipv4("http://mock-cpu:11434"));
        assert!(!url_host_is_public_ipv4(
            "http://host.docker.internal:11434"
        ));
    }
}
