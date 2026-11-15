// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reading and flipping the state of the mesh link without an `ip` subprocess.
//!
//! The mesh interface is created and configured over netlink by the WireGuard
//! backend. What it does not expose is reading the interface's addresses and
//! bringing an existing link up or changing its MTU in place, which used to be
//! a shell-out to `ip(8)`.
//!
//! That shell-out is the only part of the mesh path that needed more than
//! `CAP_NET_ADMIN` and a binary on `PATH`. iproute2 deliberately drops every
//! capability unless the effective uid is 0 (`drop_cap()`), so under a non-root
//! uid with `CAP_NET_ADMIN` granted as a file or bounding capability the
//! kernel interface is created fine and then `ip link set up` fails with
//! `Operation not permitted`. Doing the same operations from this process
//! uses the capability it was given and needs no external binary.
//!
//! Reads (`getifaddrs`) need no capability. Writes (`SIOCSIFFLAGS`,
//! `SIOCSIFMTU`) need `CAP_NET_ADMIN` in the network namespace, exactly as the
//! equivalent netlink request would.

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use nix::libc;

/// Length of `IFNAMSIZ`, including the trailing NUL.
const IFNAMSIZ: usize = 16;

/// The prefix of `struct ifreq` this module needs: the name, then the
/// request-specific union. The union is sized for the largest member
/// (`struct ifmap`, 24 bytes on LP64) so the kernel can always copy a whole
/// `ifreq` out of it.
#[repr(C)]
struct IfReq {
    name: [u8; IFNAMSIZ],
    data: [u8; 24],
}

impl IfReq {
    fn new(interface: &str) -> io::Result<Self> {
        let bytes = interface.as_bytes();
        // Leave room for the NUL the kernel expects.
        if bytes.is_empty() || bytes.len() >= IFNAMSIZ || bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid interface name {interface:?}"),
            ));
        }
        let mut name = [0u8; IFNAMSIZ];
        name[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            name,
            data: [0; 24],
        })
    }

    /// `ifr_flags` is the leading `short` of the union.
    fn flags(&self) -> i16 {
        i16::from_ne_bytes([self.data[0], self.data[1]])
    }

    fn set_flags(&mut self, flags: i16) {
        self.data[..2].copy_from_slice(&flags.to_ne_bytes());
    }

    /// `ifr_mtu` is the leading `int` of the union.
    fn set_mtu(&mut self, mtu: i32) {
        self.data[..4].copy_from_slice(&mtu.to_ne_bytes());
    }
}

fn control_socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the descriptor is owned immediately below.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a freshly created, valid descriptor nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn ioctl(fd: &OwnedFd, request: libc::c_ulong, request_data: &mut IfReq) -> io::Result<()> {
    // SAFETY: `request_data` is a live, correctly laid out `ifreq` that is at
    // least as large as the kernel's, and the descriptor is open.
    #[allow(clippy::useless_conversion)]
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, request_data as *mut IfReq) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Bring `interface` up. Idempotent: an interface that is already up is
/// written back unchanged.
pub fn set_up(interface: &str) -> io::Result<()> {
    let socket = control_socket()?;
    let mut request = IfReq::new(interface)?;
    ioctl(&socket, libc::SIOCGIFFLAGS as libc::c_ulong, &mut request)?;
    let flags = request.flags();
    if flags & libc::IFF_UP as i16 != 0 {
        return Ok(());
    }
    request.set_flags(flags | libc::IFF_UP as i16);
    ioctl(&socket, libc::SIOCSIFFLAGS as libc::c_ulong, &mut request)
}

/// Set `interface`'s MTU in place. Peers and addresses are untouched.
pub fn set_mtu(interface: &str, mtu: u32) -> io::Result<()> {
    let mtu = i32::try_from(mtu).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("MTU {mtu} out of range"),
        )
    })?;
    let socket = control_socket()?;
    let mut request = IfReq::new(interface)?;
    request.set_mtu(mtu);
    ioctl(&socket, libc::SIOCSIFMTU as libc::c_ulong, &mut request)
}

/// The IPv4 addresses on `interface` as `a.b.c.d/len`.
///
/// An interface that does not exist has no addresses; callers that need to
/// tell the two apart check for the interface first.
pub fn ipv4_addresses(interface: &str) -> io::Result<Vec<String>> {
    let mut addresses = Vec::new();
    for entry in nix::ifaddrs::getifaddrs().map_err(io::Error::from)? {
        if entry.interface_name != interface {
            continue;
        }
        let Some(address) = entry
            .address
            .as_ref()
            .and_then(|address| address.as_sockaddr_in())
        else {
            continue;
        };
        let prefix = entry
            .netmask
            .as_ref()
            .and_then(|mask| mask.as_sockaddr_in())
            .map_or(32, |mask| prefix_len(mask.ip()));
        addresses.push(format!("{}/{prefix}", address.ip()));
    }
    Ok(addresses)
}

/// Prefix length of a contiguous IPv4 netmask.
pub(crate) fn prefix_len(mask: Ipv4Addr) -> u8 {
    u32::from(mask).leading_ones() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_length_of_netmasks() {
        assert_eq!(prefix_len(Ipv4Addr::new(255, 255, 0, 0)), 16);
        assert_eq!(prefix_len(Ipv4Addr::new(255, 255, 255, 255)), 32);
        assert_eq!(prefix_len(Ipv4Addr::new(255, 255, 254, 0)), 23);
        assert_eq!(prefix_len(Ipv4Addr::new(0, 0, 0, 0)), 0);
    }

    #[test]
    fn interface_names_are_validated_before_reaching_the_kernel() {
        assert!(IfReq::new("temps-wg0").is_ok());
        assert!(IfReq::new("").is_err());
        // IFNAMSIZ is 16 including the NUL: 15 characters is the longest.
        assert!(IfReq::new("abcdefghijklmno").is_ok());
        assert!(IfReq::new("abcdefghijklmnop").is_err());
        assert!(IfReq::new("a\0b").is_err());
    }

    #[test]
    fn flags_and_mtu_share_the_union_prefix() {
        let mut request = IfReq::new("temps-wg0").unwrap();
        request.set_flags(0x1043);
        assert_eq!(request.flags(), 0x1043);
        request.set_mtu(1420);
        assert_eq!(
            i32::from_ne_bytes(request.data[..4].try_into().unwrap()),
            1420
        );
    }

    #[test]
    fn a_missing_interface_has_no_addresses() {
        assert!(ipv4_addresses("temps-nonexistent0").unwrap().is_empty());
    }

    #[test]
    fn loopback_reports_its_address_with_a_prefix() {
        let addresses = ipv4_addresses("lo").unwrap();
        assert!(
            addresses.contains(&"127.0.0.1/8".to_string()),
            "loopback addresses: {addresses:?}"
        );
    }
}
