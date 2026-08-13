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
}
