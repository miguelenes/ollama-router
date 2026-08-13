//! Resolve `loopback | tailscale | lan | all` to a bind IP. Tailscale never
//! silently falls back to 0.0.0.0.

use std::net::{IpAddr, Ipv4Addr};

use thiserror::Error;

use crate::config::{BindSpec, ListenMode};

#[derive(Debug, Error)]
pub enum ListenError {
    #[error("tailscale listen requested but no 100.64/10 IPv4 is present")]
    TailscaleMissing,
    #[error("lan listen requested but no private non-Tailscale IPv4 is present")]
    LanMissing,
    #[error("listen all requires a bearer token")]
    AllRequiresToken,
    #[error("invalid listen address: {0}")]
    InvalidAddress(String),
}

const TS_PREFIX: u8 = 100;

pub fn is_tailscale_ipv4(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    oct[0] == TS_PREFIX && (oct[1] & 0xc0) == 64
}

pub fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback()
}

/// Addresses discovered on the host (injected in tests).
pub trait AddrSource {
    fn ipv4s(&self) -> Vec<Ipv4Addr>;
}

pub struct HostAddrs;

impl AddrSource for HostAddrs {
    fn ipv4s(&self) -> Vec<Ipv4Addr> {
        let mut out = Vec::new();
        if let Ok(ifaces) = local_ipv4s() {
            out.extend(ifaces);
        }
        out
    }
}

fn local_ipv4s() -> std::io::Result<Vec<Ipv4Addr>> {
    let mut ips = Vec::new();
    // Best-effort: parse `hostname -I` / `ip -4 -o addr` is OS-specific.
    // Use std::net UDP bind trick for a primary address, plus env.
    if let Ok(s) = std::env::var("OLLAMA_NODE_AGENT_DISCOVERED_V4") {
        for part in s.split(',') {
            if let Ok(ip) = part.trim().parse() {
                ips.push(ip);
            }
        }
    }
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.connect("1.1.1.1:80");
        if let Ok(std::net::SocketAddr::V4(v4)) = sock.local_addr() {
            let ip = *v4.ip();
            if ip != Ipv4Addr::UNSPECIFIED && !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    Ok(ips)
}

/// Resolve agent or Ollama bind. `token_set` is required for mode `all`.
pub fn resolve_bind(
    spec: &BindSpec,
    addrs: &dyn AddrSource,
    token_set: bool,
) -> Result<IpAddr, ListenError> {
    match spec {
        BindSpec::Address(raw) => {
            let ip: IpAddr = raw
                .parse()
                .map_err(|_| ListenError::InvalidAddress(raw.clone()))?;
            if ip == IpAddr::V4(Ipv4Addr::UNSPECIFIED) && !token_set {
                return Err(ListenError::AllRequiresToken);
            }
            Ok(ip)
        }
        BindSpec::Mode(ListenMode::Loopback) => Ok(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        BindSpec::Mode(ListenMode::All) => {
            if !token_set {
                return Err(ListenError::AllRequiresToken);
            }
            Ok(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        }
        BindSpec::Mode(ListenMode::Tailscale) => addrs
            .ipv4s()
            .into_iter()
            .find(|ip| is_tailscale_ipv4(*ip))
            .map(IpAddr::V4)
            .ok_or(ListenError::TailscaleMissing),
        BindSpec::Mode(ListenMode::Lan) => addrs
            .ipv4s()
            .into_iter()
            .find(|ip| ip.is_private() && !is_tailscale_ipv4(*ip) && *ip != Ipv4Addr::LOCALHOST)
            .map(IpAddr::V4)
            .ok_or(ListenError::LanMissing),
    }
}

pub fn format_host_port(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v) => format!("{v}:{port}"),
        IpAddr::V6(v) => format!("[{v}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<Ipv4Addr>);
    impl AddrSource for Fixed {
        fn ipv4s(&self) -> Vec<Ipv4Addr> {
            self.0.clone()
        }
    }

    #[test]
    fn tailscale_errors_without_cgnat() {
        let addrs = Fixed(vec![Ipv4Addr::new(192, 168, 1, 10)]);
        let err = resolve_bind(&BindSpec::Mode(ListenMode::Tailscale), &addrs, false)
            .expect_err("missing ts");
        assert!(matches!(err, ListenError::TailscaleMissing));
    }

    #[test]
    fn tailscale_picks_cgnat() {
        let addrs = Fixed(vec![
            Ipv4Addr::new(192, 168, 1, 10),
            Ipv4Addr::new(100, 64, 1, 5),
        ]);
        let ip = resolve_bind(&BindSpec::Mode(ListenMode::Tailscale), &addrs, false).unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(100, 64, 1, 5)));
    }

    #[test]
    fn all_requires_token() {
        let addrs = Fixed(vec![]);
        let err = resolve_bind(&BindSpec::Mode(ListenMode::All), &addrs, false).expect_err("token");
        assert!(matches!(err, ListenError::AllRequiresToken));
        let ip = resolve_bind(&BindSpec::Mode(ListenMode::All), &addrs, true).unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn cgnat_helper() {
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 127, 255, 255)));
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(100, 63, 0, 1)));
        let _ = is_private_ipv4(Ipv4Addr::LOCALHOST);
    }
}
