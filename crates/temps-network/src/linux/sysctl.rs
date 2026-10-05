// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sysctl writes: `net.ipv4.ip_forward` and per-interface forwarding.

use crate::error::NetworkError;
use std::io::Write;

/// Enable IPv4 forwarding. Idempotent: writes `1` even when already enabled.
///
/// We deliberately do NOT touch `/etc/sysctl.conf` — persistence is the
/// operator's responsibility (or systemd-networkd's). This function only
/// affects the running kernel.
pub fn enable_ip_forward() -> crate::Result<()> {
    write_proc("/proc/sys/net/ipv4/ip_forward", b"1\n")
}

/// Whether [`enable_interface_forwarding`] found the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceForwarding {
    /// Forwarding is on for the interface (it was, or now is).
    Enabled,
    /// The interface does not exist: nothing arrives on it to forward.
    NoInterface,
}

/// Let IPv4 packets arriving on `interface` be forwarded, and stop ICMP
/// redirects out of it. Idempotent; writes only what differs.
///
/// There is deliberately no way to turn it off here. The kernel checks the
/// *ingress* interface's `forwarding` for every routed packet
/// (`ip_route_input_slow` refuses with `IN_DEV_FORWARD(in_dev)` unset), and
/// a DNAT'd connection to a published container port is routed: with it
/// off on `temps-wg0`, nothing reaches a container over the mesh. So every
/// member forwards, and which forwards are allowed (a hub's member-to-member
/// relaying, everyone's published ports) is the firewall's decision, not
/// this switch's.
///
/// Redirects go off because a hub sends relayed packets back out the
/// interface they came in on, exactly when the kernel would emit one, and
/// it does so before the firewall's forward hook sees the packet.
pub fn enable_interface_forwarding(interface: &str) -> crate::Result<InterfaceForwarding> {
    if !std::path::Path::new(&interface_conf_dir(interface)).is_dir() {
        return Ok(InterfaceForwarding::NoInterface);
    }
    for (path, value) in interface_forwarding_writes(interface) {
        write_proc_if_changed(&path, value)?;
    }
    Ok(InterfaceForwarding::Enabled)
}

fn interface_conf_dir(interface: &str) -> String {
    format!("/proc/sys/net/ipv4/conf/{interface}")
}

/// The values [`enable_interface_forwarding`] writes, in order.
fn interface_forwarding_writes(interface: &str) -> [(String, &'static [u8]); 2] {
    let dir = interface_conf_dir(interface);
    [
        (format!("{dir}/forwarding"), b"1\n"),
        (format!("{dir}/send_redirects"), b"0\n"),
    ]
}

/// Write `value` unless the file already holds it: these are checked on
/// every mesh tick, and most ticks change nothing.
fn write_proc_if_changed(path: &str, value: &[u8]) -> crate::Result<()> {
    match std::fs::read(path) {
        Ok(current) if current.trim_ascii() == value.trim_ascii() => Ok(()),
        _ => write_proc(path, value),
    }
}

fn write_proc(path: &str, value: &[u8]) -> crate::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| NetworkError::Io {
            op: "open",
            path: path.into(),
            reason: e.to_string(),
        })?;
    f.write_all(value).map_err(|e| NetworkError::Io {
        op: "write",
        path: path.into(),
        reason: e.to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_member_forwards_on_the_mesh_and_sends_no_redirects() {
        // The same writes whatever the host's role: forwarding off would
        // drop DNAT'd traffic to published ports arriving on the tunnel.
        assert_eq!(
            interface_forwarding_writes("temps-wg0"),
            [
                (
                    "/proc/sys/net/ipv4/conf/temps-wg0/forwarding".to_string(),
                    &b"1\n"[..]
                ),
                (
                    "/proc/sys/net/ipv4/conf/temps-wg0/send_redirects".to_string(),
                    &b"0\n"[..]
                ),
            ]
        );
    }

    #[test]
    fn a_missing_interface_is_reported_rather_than_written() {
        assert_eq!(
            enable_interface_forwarding("temps-test-absent-if0").unwrap(),
            InterfaceForwarding::NoInterface
        );
    }
}
