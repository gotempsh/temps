// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The host's own DNS configuration, read from `resolv.conf`.
//!
//! Two consumers need it, from two different network namespaces:
//!
//! * containers get the host's servers as fallbacks *after* the cluster
//!   resolver ([`dns_with_fallback`]), so the resolver is never a single point
//!   of failure for outbound DNS. Loopback servers are dropped there: inside a
//!   container's netns they point at the container itself.
//! * the resolver forwards names outside `temps.local` to the host's servers
//!   first ([`host_upstream_resolvers`]). It runs in the host netns, so a
//!   loopback stub such as systemd-resolved's `127.0.0.53` is reachable and is
//!   exactly what the host itself uses. Forwarding to hard-coded public
//!   resolvers instead made every external lookup slow, or fail, on hosts
//!   whose egress to them is filtered -- and leaked queries that operators
//!   had deliberately kept internal.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Where the host keeps its resolver configuration. systemd-resolved hosts
/// point `/etc/resolv.conf` at the `127.0.0.53` stub and expose the real
/// upstream servers in the second file, which is also what Docker's embedded
/// DNS reads for the same reason.
const RESOLV_CONF_PATHS: [&str; 2] = ["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf"];

/// Last-resort upstreams, used only after every server the host configured.
pub const PUBLIC_FALLBACK_RESOLVERS: [Ipv4Addr; 3] = [
    Ipv4Addr::new(1, 1, 1, 1),
    Ipv4Addr::new(1, 0, 0, 1),
    Ipv4Addr::new(8, 8, 8, 8),
];

/// glibc and musl ignore anything past the third `nameserver` line.
const MAX_CONTAINER_NAMESERVERS: usize = 3;

/// Every `nameserver` entry in resolv.conf-formatted text, in order.
/// Malformed entries are skipped.
pub fn parse_nameservers(resolv_conf: &str) -> Vec<IpAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.trim().parse::<IpAddr>().ok())
        .collect()
}

/// [`parse_nameservers`] without loopback addresses, which are meaningless
/// inside a container's own network namespace.
pub fn parse_non_loopback_nameservers(resolv_conf: &str) -> Vec<String> {
    parse_nameservers(resolv_conf)
        .into_iter()
        .filter(|ip| !ip.is_loopback())
        .map(|ip| ip.to_string())
        .collect()
}

/// The host's own non-loopback DNS servers, for use inside containers.
/// Empty when no readable resolv.conf names one.
pub fn host_default_dns_servers() -> Vec<String> {
    for path in RESOLV_CONF_PATHS {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let servers = parse_non_loopback_nameservers(&contents);
            if !servers.is_empty() {
                return servers;
            }
        }
    }
    Vec::new()
}

/// Appends entries from `fallback_pool` to `primary`, deduplicated and
/// capped at three entries.
pub fn merge_dns_with_fallback(mut primary: Vec<String>, fallback_pool: &[String]) -> Vec<String> {
    for fallback in fallback_pool {
        if primary.len() >= MAX_CONTAINER_NAMESERVERS {
            break;
        }
        if !primary.contains(fallback) {
            primary.push(fallback.clone());
        }
    }
    primary
}

/// Appends the host's own DNS servers to `primary` (the cluster resolver),
/// so the resolver is never the only nameserver a container has.
pub fn dns_with_fallback(primary: Vec<String>) -> Vec<String> {
    merge_dns_with_fallback(primary, &host_default_dns_servers())
}

/// Upstreams for the resolver's forwarder: the host's configured servers
/// first, then [`PUBLIC_FALLBACK_RESOLVERS`]. `own_addresses` are the
/// resolver's listen addresses, never forwarded to (a host whose resolv.conf
/// names the bridge gateway would otherwise loop queries back into itself).
pub fn upstream_resolvers_from(
    host_servers: &[IpAddr],
    own_addresses: &[IpAddr],
) -> Vec<SocketAddr> {
    let mut upstreams: Vec<IpAddr> = Vec::new();
    let public = PUBLIC_FALLBACK_RESOLVERS.iter().map(|ip| IpAddr::V4(*ip));
    for ip in host_servers.iter().copied().chain(public) {
        if ip.is_unspecified() || own_addresses.contains(&ip) || upstreams.contains(&ip) {
            continue;
        }
        upstreams.push(ip);
    }
    upstreams
        .into_iter()
        .map(|ip| SocketAddr::new(ip, 53))
        .collect()
}

/// [`upstream_resolvers_from`] with the servers read from this host. The
/// first file that names any server wins, loopback stubs included: the
/// resolver shares the host's network namespace.
pub fn host_upstream_resolvers(own_addresses: &[IpAddr]) -> Vec<SocketAddr> {
    let host_servers = RESOLV_CONF_PATHS
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|contents| parse_nameservers(&contents))
        .find(|servers| !servers.is_empty())
        .unwrap_or_default();
    upstream_resolvers_from(&host_servers, own_addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parse_non_loopback_nameservers_skips_systemd_resolved_stub() {
        let resolv_conf = "nameserver 127.0.0.53\noptions edns0 trust-ad\n";
        assert!(parse_non_loopback_nameservers(resolv_conf).is_empty());
    }

    #[test]
    fn parse_non_loopback_nameservers_keeps_real_servers() {
        let resolv_conf = "nameserver 10.0.0.53\nnameserver 8.8.8.8\nsearch example.com\n";
        assert_eq!(
            parse_non_loopback_nameservers(resolv_conf),
            vec!["10.0.0.53", "8.8.8.8"]
        );
    }

    #[test]
    fn parse_non_loopback_nameservers_skips_ipv6_loopback_and_malformed() {
        let resolv_conf = "nameserver ::1\nnameserver not-an-ip\nnameserver 2001:4860:4860::8888\n";
        assert_eq!(
            parse_non_loopback_nameservers(resolv_conf),
            vec!["2001:4860:4860::8888"]
        );
    }

    #[test]
    fn merge_dns_with_fallback_appends_pool_after_primary() {
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string()],
            &["10.0.0.53".to_string(), "10.0.0.54".to_string()],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53", "10.0.0.54"]);
    }

    #[test]
    fn merge_dns_with_fallback_caps_at_three() {
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string()],
            &[
                "10.0.0.53".to_string(),
                "10.0.0.54".to_string(),
                "10.0.0.55".to_string(),
            ],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53", "10.0.0.54"]);
    }

    #[test]
    fn merge_dns_with_fallback_dedupes_against_primary() {
        let merged = merge_dns_with_fallback(
            vec!["172.18.0.1".to_string(), "10.0.0.53".to_string()],
            &["10.0.0.53".to_string()],
        );
        assert_eq!(merged, vec!["172.18.0.1", "10.0.0.53"]);
    }

    #[test]
    fn merge_dns_with_fallback_empty_pool_leaves_primary_untouched() {
        let merged = merge_dns_with_fallback(vec!["172.18.0.1".to_string()], &[]);
        assert_eq!(merged, vec!["172.18.0.1"]);
    }

    #[test]
    fn the_resolver_forwards_to_the_host_servers_before_public_ones() {
        let upstreams = upstream_resolvers_from(&[ip("127.0.0.53"), ip("10.0.0.2")], &[]);
        let ips: Vec<IpAddr> = upstreams.iter().map(SocketAddr::ip).collect();
        assert_eq!(
            ips,
            vec![
                ip("127.0.0.53"),
                ip("10.0.0.2"),
                ip("1.1.1.1"),
                ip("1.0.0.1"),
                ip("8.8.8.8")
            ]
        );
        assert!(upstreams.iter().all(|addr| addr.port() == 53));
    }

    #[test]
    fn the_resolver_never_forwards_to_itself() {
        // A host whose resolv.conf names the bridge gateway the resolver
        // listens on must not loop queries back into it.
        let upstreams =
            upstream_resolvers_from(&[ip("172.19.0.1"), ip("0.0.0.0")], &[ip("172.19.0.1")]);
        let ips: Vec<IpAddr> = upstreams.iter().map(SocketAddr::ip).collect();
        assert_eq!(ips, vec![ip("1.1.1.1"), ip("1.0.0.1"), ip("8.8.8.8")]);
    }

    #[test]
    fn a_host_server_that_is_also_a_public_fallback_appears_once() {
        let upstreams = upstream_resolvers_from(&[ip("8.8.8.8")], &[]);
        let ips: Vec<IpAddr> = upstreams.iter().map(SocketAddr::ip).collect();
        assert_eq!(ips, vec![ip("8.8.8.8"), ip("1.1.1.1"), ip("1.0.0.1")]);
    }
}
