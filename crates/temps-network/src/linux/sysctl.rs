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

/// Whether IPv4 packets arriving on `interface` may be forwarded. The kernel
/// decides forwarding by the interface a packet comes in on, so this opens
/// forwarding for that interface only. Enabling also stops ICMP redirects
/// out of it: a hub sends relayed packets back out the interface they came
/// in on, which is exactly when the kernel would otherwise emit them.
pub fn set_interface_forwarding(interface: &str, enabled: bool) -> crate::Result<()> {
    let value: &[u8] = if enabled { b"1\n" } else { b"0\n" };
    write_proc_if_changed(
        &format!("/proc/sys/net/ipv4/conf/{interface}/forwarding"),
        value,
    )?;
    if enabled {
        write_proc_if_changed(
            &format!("/proc/sys/net/ipv4/conf/{interface}/send_redirects"),
            b"0\n",
        )?;
    }
    Ok(())
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
