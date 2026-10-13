// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Classifying the address a cluster node registered with.

use std::net::{IpAddr, SocketAddr};

/// Whether a node's registered address (`ip` or `ip:port`) is private: RFC
/// 1918 or IPv6 unique-local. Only such an address may carry published
/// workload ports; a node that joined with anything else (a public IP, a
/// CGNAT/Tailscale 100.64/10 address) reaches its workloads over the
/// WireGuard mesh instead.
pub fn is_private_node_address(address: &str) -> bool {
    let address = address.trim();
    let ip = address
        .parse::<IpAddr>()
        .or_else(|_| address.parse::<SocketAddr>().map(|socket| socket.ip()));
    match ip {
        Ok(IpAddr::V4(v4)) => v4.is_private(),
        Ok(IpAddr::V6(v6)) => v6.is_unique_local(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_private_node_address;

    #[test]
    fn only_rfc1918_and_unique_local_addresses_are_private() {
        for private in [
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.9",
            "fd00::1",
            "10.0.0.5:3100",
            "[fd00::1]:3100",
        ] {
            assert!(is_private_node_address(private), "{private}");
        }
        for public in [
            "203.0.113.10",
            "8.8.8.8",
            "100.101.102.103",
            "2001:db8::1",
            "not-an-ip",
            "",
        ] {
            assert!(!is_private_node_address(public), "{public}");
        }
    }
}
