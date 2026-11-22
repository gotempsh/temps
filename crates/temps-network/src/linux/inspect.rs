// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Reading what the kernel reports about a link, for comparing an existing
//! VXLAN device with the one requested.
//!
//! Links are read as rtnetlink messages, the same interface devices are
//! created through, and not by parsing `ip -d link show` text: that output
//! depends on the `iproute2` build installed on the host, which has been seen
//! to crash outright on a vxlan device whose kernel reports an attribute it
//! does not know, leaving an empty error and a device that can never be
//! inspected. Attributes this crate does not know either are kept as opaque
//! values by the netlink decoder, so a newer kernel cannot break the read.
//!
//! Kept free of rtnetlink's socket types so it can be tested on decoded
//! messages alone.

use netlink_packet_route::link::{
    InfoData, InfoKind, InfoVxlan, LinkAttribute, LinkInfo, LinkMessage,
};
use std::fmt;
use std::net::IpAddr;
use std::process::Output;

/// A link, as needed to compare it with a requested VXLAN device and to
/// rebuild it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDescription {
    pub index: u32,
    pub name: Option<String>,
    pub mtu: Option<u32>,
    /// Index of the bridge it is enslaved to.
    pub controller: Option<u32>,
    /// `IFLA_INFO_KIND`, e.g. `vxlan` or `bridge`; `None` for a link without
    /// one, such as a physical NIC.
    pub kind: Option<String>,
    /// Set when the link is a VXLAN device.
    pub vxlan: Option<VxlanAttributes>,
}

/// The topology of a VXLAN device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VxlanAttributes {
    pub vni: Option<u32>,
    pub port: Option<u16>,
    /// Index of the underlay device it sends through; `None` when it is not
    /// bound to one.
    pub parent: Option<u32>,
    /// The source address of what it encapsulates; `None` for a device
    /// created before the local address was set.
    pub local: Option<IpAddr>,
}

impl LinkDescription {
    pub fn from_message(message: &LinkMessage) -> Self {
        let mut link = Self {
            index: message.header.index,
            name: None,
            mtu: None,
            controller: None,
            kind: None,
            vxlan: None,
        };
        for attribute in &message.attributes {
            match attribute {
                LinkAttribute::IfName(name) => link.name = Some(name.clone()),
                LinkAttribute::Mtu(mtu) => link.mtu = Some(*mtu),
                LinkAttribute::Controller(index) if *index != 0 => link.controller = Some(*index),
                LinkAttribute::LinkInfo(infos) => {
                    for info in infos {
                        match info {
                            LinkInfo::Kind(InfoKind::Vxlan) => {
                                link.kind = Some(InfoKind::Vxlan.to_string());
                                link.vxlan.get_or_insert(VxlanAttributes {
                                    vni: None,
                                    port: None,
                                    parent: None,
                                    local: None,
                                });
                            }
                            LinkInfo::Kind(kind) => link.kind = Some(kind.to_string()),
                            LinkInfo::Data(InfoData::Vxlan(data)) => {
                                link.vxlan = Some(vxlan_attributes(data));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        link
    }

    /// The link's name, or its index when the kernel did not report one.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("index {}", self.index))
    }

    /// Whether this is a VXLAN device with `vni`, `port` and the underlay at
    /// `parent` (`None`: the requested underlay does not exist, so nothing
    /// matches it). `Err` when it is not a VXLAN device at all: something
    /// else owns the name, and deleting it would be wrong.
    pub fn matches_topology(
        &self,
        parent: Option<u32>,
        vni: u32,
        port: u16,
    ) -> Result<bool, String> {
        let Some(vxlan) = self.vxlan else {
            return Err(format!(
                "existing device is not VXLAN: {self} (kind {})",
                self.kind.as_deref().unwrap_or("none")
            ));
        };
        Ok(parent.is_some()
            && vxlan.parent == parent
            && vxlan.vni == Some(vni)
            && vxlan.port == Some(port))
    }

    /// Whether a VXLAN device sends from `local`. A device without one, as
    /// every one created before it was set, does not.
    pub fn has_local(&self, local: IpAddr) -> bool {
        self.vxlan.is_some_and(|vxlan| vxlan.local == Some(local))
    }

    /// Whether this is a VXLAN device other than `name` holding `vni` on
    /// `port`: the kernel refuses a second one.
    pub fn holds(&self, name: &str, vni: u32, port: u16) -> bool {
        self.name.as_deref() != Some(name)
            && self
                .vxlan
                .is_some_and(|vxlan| vxlan.vni == Some(vni) && vxlan.port == Some(port))
    }
}

fn vxlan_attributes(data: &[InfoVxlan]) -> VxlanAttributes {
    let mut vxlan = VxlanAttributes {
        vni: None,
        port: None,
        parent: None,
        local: None,
    };
    for attribute in data {
        match attribute {
            InfoVxlan::Id(vni) => vxlan.vni = Some(*vni),
            InfoVxlan::Port(port) => vxlan.port = Some(*port),
            InfoVxlan::Link(index) if *index != 0 => vxlan.parent = Some(*index),
            // The kernel reports an unset local address as all zeros.
            InfoVxlan::Local(local) if !local.is_unspecified() => {
                vxlan.local = Some(IpAddr::V4(*local));
            }
            InfoVxlan::Local6(local) if !local.is_unspecified() => {
                vxlan.local = Some(IpAddr::V6(*local));
            }
            _ => {}
        }
    }
    vxlan
}

/// For logs and errors: `vxlan-temps0 (index 11): vxlan id 42 local
/// 10.0.0.1 dev index 3 dstport 4789 mtu 1450`.
impl fmt::Display for LinkDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (index {})",
            self.name.as_deref().unwrap_or("?"),
            self.index
        )?;
        if let Some(vxlan) = self.vxlan {
            write!(f, ": vxlan")?;
            if let Some(vni) = vxlan.vni {
                write!(f, " id {vni}")?;
            }
            match vxlan.local {
                Some(local) => write!(f, " local {local}")?,
                None => write!(f, " no local")?,
            }
            match vxlan.parent {
                Some(parent) => write!(f, " dev index {parent}")?,
                None => write!(f, " no dev")?,
            }
            if let Some(port) = vxlan.port {
                write!(f, " dstport {port}")?;
            }
        }
        if let Some(mtu) = self.mtu {
            write!(f, " mtu {mtu}")?;
        }
        Ok(())
    }
}

/// Why a command failed: its stderr and its exit status. The status is what
/// tells a crash apart, which leaves stderr empty: `signal: 11 (SIGSEGV)`.
pub fn command_failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("{} with no error output", output.status)
    } else {
        format!("{stderr} ({})", output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::{command_failure, LinkDescription, VxlanAttributes};
    use netlink_packet_core::Parseable;
    use netlink_packet_route::link::LinkMessage;
    use std::net::IpAddr;

    /// A netlink attribute: length, type, payload, padded to 4 bytes.
    fn nla(kind: u16, payload: &[u8]) -> Vec<u8> {
        let len = 4 + payload.len();
        let mut bytes = Vec::with_capacity(len.next_multiple_of(4));
        bytes.extend_from_slice(&(len as u16).to_ne_bytes());
        bytes.extend_from_slice(&kind.to_ne_bytes());
        bytes.extend_from_slice(payload);
        bytes.resize(len.next_multiple_of(4), 0);
        bytes
    }

    /// An `RTM_NEWLINK` payload as the kernel sends it for a get by name.
    fn link_message(index: u32, attributes: &[Vec<u8>]) -> LinkMessage {
        // ifinfomsg: family, pad, type (ARPHRD_ETHER), index, flags, change.
        let mut bytes = vec![0u8, 0];
        bytes.extend_from_slice(&1u16.to_ne_bytes());
        bytes.extend_from_slice(&index.to_ne_bytes());
        bytes.extend_from_slice(&0x1043u32.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        for attribute in attributes {
            bytes.extend_from_slice(attribute);
        }
        LinkMessage::parse(&bytes[..]).expect("a well-formed link message decodes")
    }

    const IFLA_IFNAME: u16 = 3;
    const IFLA_MTU: u16 = 4;
    const IFLA_MASTER: u16 = 10;
    const IFLA_LINKINFO: u16 = 18;
    const IFLA_INFO_KIND: u16 = 1;
    const IFLA_INFO_DATA: u16 = 2;

    const IFLA_VXLAN_GROUP: u16 = 2;
    const IFLA_VXLAN_LOCAL: u16 = 4;
    const IFLA_VXLAN_LOCAL6: u16 = 17;

    fn local() -> IpAddr {
        IpAddr::from([10, 0, 0, 1])
    }

    /// `vxlan-temps0` with VNI 42 on port 4789 from 10.0.0.1 over the link at
    /// index 3, enslaved to the bridge at index 5, reported with `extra` VXLAN
    /// attributes.
    fn vxlan_device(extra: &[Vec<u8>]) -> LinkMessage {
        vxlan_device_from(&[nla(IFLA_VXLAN_LOCAL, &[10, 0, 0, 1])], extra)
    }

    /// [`vxlan_device`] with the local-address attributes `local` instead.
    fn vxlan_device_from(local: &[Vec<u8>], extra: &[Vec<u8>]) -> LinkMessage {
        let mut data = vec![
            nla(1, &42u32.to_ne_bytes()), // IFLA_VXLAN_ID
            nla(3, &3u32.to_ne_bytes()),  // IFLA_VXLAN_LINK
        ];
        data.extend_from_slice(local);
        data.extend_from_slice(&[
            nla(7, &[0]),                    // IFLA_VXLAN_LEARNING
            nla(15, &4789u16.to_be_bytes()), // IFLA_VXLAN_PORT, network order
            nla(10, &[0, 0, 0, 0]),          // IFLA_VXLAN_PORT_RANGE
        ]);
        data.extend_from_slice(extra);
        let info = [
            nla(IFLA_INFO_KIND, b"vxlan\0"),
            nla(IFLA_INFO_DATA, &data.concat()),
        ];
        link_message(
            11,
            &[
                nla(IFLA_IFNAME, b"vxlan-temps0\0"),
                nla(IFLA_MTU, &1450u32.to_ne_bytes()),
                nla(IFLA_MASTER, &5u32.to_ne_bytes()),
                nla(IFLA_LINKINFO, &info.concat()),
            ],
        )
    }

    fn expected_vxlan() -> LinkDescription {
        LinkDescription {
            index: 11,
            name: Some("vxlan-temps0".into()),
            mtu: Some(1450),
            controller: Some(5),
            kind: Some("vxlan".into()),
            vxlan: Some(VxlanAttributes {
                vni: Some(42),
                port: Some(4789),
                parent: Some(3),
                local: Some(local()),
            }),
        }
    }

    #[test]
    fn a_vxlan_device_is_read_from_its_link_message() {
        let link = LinkDescription::from_message(&vxlan_device(&[]));
        assert_eq!(link, expected_vxlan());
        assert_eq!(link.matches_topology(Some(3), 42, 4789), Ok(true));
        assert!(link.has_local(local()));
    }

    /// The regression: a kernel that reports VXLAN attributes newer than the
    /// tools reading them. Kernel 7.0 adds `IFLA_VXLAN_RESERVED_BITS` (33),
    /// which a distribution `ip` crashed on, so the device could never be
    /// inspected again. Read over netlink, it and anything newer still (35
    /// here, unknown to the decoder) leave the topology readable.
    #[test]
    fn newer_kernel_vxlan_attributes_do_not_prevent_reading_the_device() {
        let message = vxlan_device(&[
            nla(33, &0u64.to_be_bytes()), // IFLA_VXLAN_RESERVED_BITS
            nla(34, &[0]),                // IFLA_VXLAN_MC_ROUTE
            nla(35, &[1, 2, 3, 4, 5, 6]), // not defined yet
        ]);
        let link = LinkDescription::from_message(&message);
        assert_eq!(link, expected_vxlan());
        assert_eq!(link.matches_topology(Some(3), 42, 4789), Ok(true));
    }

    #[test]
    fn a_vxlan_without_the_local_address_is_told_apart() {
        // Every device created before `local` was set: no attribute, or the
        // kernel's all-zero address.
        for unset in [vec![], vec![nla(IFLA_VXLAN_LOCAL, &[0, 0, 0, 0])]] {
            let legacy = LinkDescription::from_message(&vxlan_device_from(&unset, &[]));
            assert_eq!(legacy.vxlan.and_then(|vxlan| vxlan.local), None);
            assert!(!legacy.has_local(local()));
            // The rest of its topology still matches: only the address is off.
            assert_eq!(legacy.matches_topology(Some(3), 42, 4789), Ok(true));
            assert!(legacy.to_string().contains("no local"), "{legacy}");
        }
        // Another address.
        assert!(!expected_vxlan().has_local(IpAddr::from([10, 0, 0, 2])));
        // A remote (group) address is not a local one.
        let remote = LinkDescription::from_message(&vxlan_device_from(
            &[nla(IFLA_VXLAN_GROUP, &[10, 0, 0, 1])],
            &[],
        ));
        assert!(!remote.has_local(local()));
    }

    #[test]
    fn an_ipv6_local_address_is_read() {
        let address: std::net::Ipv6Addr = "fd00::1".parse().expect("an IPv6 address");
        let link = LinkDescription::from_message(&vxlan_device_from(
            &[nla(IFLA_VXLAN_LOCAL6, &address.octets())],
            &[],
        ));
        let expanded: IpAddr = "fd00:0:0:0:0:0:0:1".parse().expect("an IPv6 address");
        assert!(link.has_local(expanded));
        assert!(!link.has_local(local()));
        assert!(link.to_string().contains("local fd00::1"), "{link}");
    }

    #[test]
    fn a_vxlan_on_another_parent_vni_or_port_differs() {
        let link = expected_vxlan();
        // The underlay moved, e.g. onto the WireGuard mesh.
        assert_eq!(link.matches_topology(Some(7), 42, 4789), Ok(false));
        // The requested underlay does not exist.
        assert_eq!(link.matches_topology(None, 42, 4789), Ok(false));
        assert_eq!(link.matches_topology(Some(3), 99, 4789), Ok(false));
        assert_eq!(link.matches_topology(Some(3), 42, 8472), Ok(false));
    }

    #[test]
    fn a_vxlan_without_a_parent_differs() {
        let info = [
            nla(IFLA_INFO_KIND, b"vxlan\0"),
            nla(
                IFLA_INFO_DATA,
                &[
                    nla(1, &42u32.to_ne_bytes()),
                    nla(15, &4789u16.to_be_bytes()),
                ]
                .concat(),
            ),
        ];
        let link = LinkDescription::from_message(&link_message(
            11,
            &[
                nla(IFLA_IFNAME, b"vxlan-temps0\0"),
                nla(IFLA_LINKINFO, &info.concat()),
            ],
        ));
        assert_eq!(link.vxlan.and_then(|vxlan| vxlan.parent), None);
        assert_eq!(link.matches_topology(Some(3), 42, 4789), Ok(false));
        assert!(link.to_string().contains("no dev"), "{link}");
    }

    #[test]
    fn a_device_that_is_not_vxlan_is_an_error() {
        let bridge = LinkDescription::from_message(&link_message(
            11,
            &[
                nla(IFLA_IFNAME, b"vxlan-temps0\0"),
                nla(IFLA_LINKINFO, &nla(IFLA_INFO_KIND, b"bridge\0")),
            ],
        ));
        let error = bridge
            .matches_topology(Some(3), 42, 4789)
            .expect_err("a bridge is not a VXLAN device");
        assert!(error.contains("not VXLAN"), "{error}");
        assert!(error.contains("kind bridge"), "{error}");
        assert!(error.contains("vxlan-temps0"), "{error}");

        // A physical NIC reports no kind at all.
        let nic = LinkDescription::from_message(&link_message(2, &[nla(IFLA_IFNAME, b"eth0\0")]));
        assert_eq!(nic.kind, None);
        assert!(nic.matches_topology(Some(3), 42, 4789).is_err());
    }

    #[test]
    fn another_device_holding_the_vni_and_port_is_found() {
        let ours = expected_vxlan();
        // The device being replaced does not block its own replacement.
        assert!(!ours.holds("vxlan-temps0", 42, 4789));
        assert!(ours.holds("vx0", 42, 4789));
        assert!(!ours.holds("vx0", 42, 4790));
        assert!(!ours.holds("vx0", 7, 4789));
        let nic = LinkDescription {
            vxlan: None,
            kind: None,
            ..expected_vxlan()
        };
        assert!(!nic.holds("vx0", 42, 4789));
    }

    /// A crash leaves stderr empty; the status is what says what happened,
    /// instead of an error that ends at its colon.
    #[test]
    fn a_crashed_command_is_reported_by_its_signal() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{ExitStatus, Output};
        let crashed = Output {
            // Raw wait status of a process killed by SIGSEGV (11).
            status: ExitStatus::from_raw(11),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let reason = command_failure(&crashed);
        assert!(reason.contains("signal: 11"), "{reason}");
        assert!(reason.contains("SIGSEGV"), "{reason}");

        let refused = Output {
            status: ExitStatus::from_raw(2 << 8),
            stdout: Vec::new(),
            stderr: b"RTNETLINK answers: No such device\n".to_vec(),
        };
        assert_eq!(
            command_failure(&refused),
            "RTNETLINK answers: No such device (exit status: 2)"
        );
    }
}
