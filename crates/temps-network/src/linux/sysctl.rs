//! Sysctl writes — currently just `net.ipv4.ip_forward`.

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
