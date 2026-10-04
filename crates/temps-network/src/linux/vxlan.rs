// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! VXLAN device + FDB management.
//!
//! Device creation goes through rtnetlink. FDB entries (`bridge fdb append`)
//! are managed via the `bridge` command-line tool because rtnetlink's FDB
//! support is awkward and `bridge` is part of `iproute2` which is installed
//! on every Linux distribution we care about.

use crate::error::NetworkError;
use crate::linux::bridge::link_index_by_name;
use rtnetlink::{Handle, LinkUnspec, LinkVxlan};
use std::net::IpAddr;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Ensure that a VXLAN device with the given name and parameters exists,
/// has the right MTU, and is up. Idempotent: if the device exists already
/// and is compatible, this is a no-op.
pub async fn ensure(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
    if let Some(idx) = link_index_by_name(handle, name).await? {
        debug!(vxlan = %name, idx, "vxlan device already exists");
        match existing_topology(name, underlay_dev, vni, port).await? {
            Topology::Matches => {}
            Topology::Differs(detail) => {
                // Our device, built for another parent, VNI or port — e.g. the
                // underlay moved onto the WireGuard mesh. A VXLAN device's
                // parent can't be changed in place, so replace it; bootstrap
                // re-enslaves it and repopulates the FDB right after.
                warn!(vxlan = %name, parent = %underlay_dev, vni, port, existing = %detail, "recreating vxlan device for a changed topology");
                return replace(handle, idx, &detail, name, underlay_dev, vni, port, mtu).await;
            }
        }
        handle
            .link()
            .set(LinkUnspec::new_with_index(idx).mtu(mtu).build())
            .execute()
            .await
            .map_err(|e| NetworkError::Vxlan {
                device: name.into(),
                reason: format!("set_mtu: {}", e),
            })?;
        handle
            .link()
            .set(LinkUnspec::new_with_index(idx).up().build())
            .execute()
            .await
            .map_err(|e| NetworkError::Vxlan {
                device: name.into(),
                reason: format!("link_up: {}", e),
            })?;
        return Ok(idx);
    }
    create(handle, name, underlay_dev, vni, port, mtu).await
}

/// Replace the device at `old_index` (whose `ip -d -o link show` detail is
/// `previous_detail`) with one of the requested topology.
///
/// The kernel refuses a second VXLAN device with the same VNI and port, so
/// the replacement cannot be built beside the device it replaces: when only
/// the parent moves, which is the usual case, the two always collide. So the
/// old device is deleted first and the new one created under its name. What
/// can be checked beforehand is (the new parent exists); if the kernel still
/// refuses the replacement, the previous device is rebuilt as it was — same
/// topology, bridge and FDB — so the overlay keeps running. A process that
/// stops between the two steps leaves no device under the name, and the next
/// bootstrap simply creates it.
#[allow(clippy::too_many_arguments)]
async fn replace(
    handle: &Handle,
    old_index: u32,
    previous_detail: &str,
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
    if link_index_by_name(handle, underlay_dev).await?.is_none() {
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "underlay device '{underlay_dev}' not found, so no replacement for \
                 vni={vni}, port={port} can be built; the existing device is kept"
            ),
        });
    }
    let previous = Previous::capture(name, previous_detail).await;

    delete(handle, old_index, name, "delete for replacement").await?;
    match create(handle, name, underlay_dev, vni, port, mtu).await {
        Ok(index) => {
            info!(vxlan = %name, vni, port, parent = %underlay_dev, "vxlan device replaced");
            Ok(index)
        }
        Err(error) => {
            let restored = match previous {
                Some(previous) => previous.restore(handle, name, mtu).await,
                None => Err("its previous topology could not be read".to_string()),
            };
            let outcome = match restored {
                Ok(()) => "the previous device was restored".to_string(),
                Err(reason) => format!("restoring the previous device also failed: {reason}"),
            };
            Err(NetworkError::Vxlan {
                device: name.into(),
                reason: format!(
                    "could not build its replacement for parent={underlay_dev}, vni={vni}, \
                     port={port} ({error}); {outcome}"
                ),
            })
        }
    }
}

/// A device about to be replaced, as needed to rebuild it.
#[derive(Debug, PartialEq, Eq)]
struct Previous {
    parent: String,
    vni: u32,
    port: u16,
    master: Option<String>,
    fdb: Vec<IpAddr>,
}

impl Previous {
    async fn capture(name: &str, detail: &str) -> Option<Self> {
        let Some(mut previous) = parse_previous(detail) else {
            warn!(vxlan = %name, %detail, "could not read the topology of the device being replaced; it cannot be restored if its replacement fails");
            return None;
        };
        match Command::new("bridge")
            .args(["fdb", "show", "dev", name])
            .stdin(Stdio::null())
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                previous.fdb = parse_fdb_destinations(&String::from_utf8_lossy(&output.stdout));
            }
            Ok(output) => {
                warn!(vxlan = %name, error = %String::from_utf8_lossy(&output.stderr).trim(), "could not read the FDB of the device being replaced");
            }
            Err(error) => {
                warn!(vxlan = %name, %error, "could not read the FDB of the device being replaced");
            }
        }
        Some(previous)
    }

    async fn restore(&self, handle: &Handle, name: &str, mtu: u32) -> Result<(), String> {
        create(handle, name, &self.parent, self.vni, self.port, mtu)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(master) = &self.master {
            enslave_to_bridge(handle, name, master)
                .await
                .map_err(|error| error.to_string())?;
        }
        for dst in &self.fdb {
            add_fdb(handle, name, *dst)
                .await
                .map_err(|error| error.to_string())?;
        }
        warn!(vxlan = %name, parent = %self.parent, vni = self.vni, port = self.port, "restored the previous vxlan device after a failed replacement");
        Ok(())
    }
}

/// The parent, VNI, port and bridge of a VXLAN device from its
/// `ip -d -o link show` detail. The FDB is read separately.
fn parse_previous(detail: &str) -> Option<Previous> {
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    let vxlan_index = tokens.iter().position(|token| *token == "vxlan")?;
    let (link, topology) = tokens.split_at(vxlan_index);
    let value = |section: &[&str], key: &str| {
        section
            .windows(2)
            .find(|pair| pair[0] == key)
            .map(|pair| pair[1].to_string())
    };
    Some(Previous {
        parent: value(topology, "dev")?,
        vni: value(topology, "id")?.parse().ok()?,
        port: value(topology, "dstport")?.parse().ok()?,
        master: value(link, "master"),
        fdb: Vec::new(),
    })
}

/// Destinations of the default-flood (all-zero MAC) entries in
/// `bridge fdb show dev <vxlan>` output: the ones [`add_fdb`] writes.
fn parse_fdb_destinations(output: &str) -> Vec<IpAddr> {
    output
        .lines()
        .filter_map(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.first() != Some(&"00:00:00:00:00:00") {
                return None;
            }
            let dst = tokens.windows(2).find(|pair| pair[0] == "dst")?[1];
            dst.parse().ok()
        })
        .collect()
}

async fn delete(handle: &Handle, index: u32, name: &str, step: &str) -> crate::Result<()> {
    handle
        .link()
        .del(index)
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("{step}: {e}"),
        })
}

async fn create(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
    let parent_index =
        link_index_by_name(handle, underlay_dev)
            .await?
            .ok_or(NetworkError::Vxlan {
                device: name.into(),
                reason: format!("underlay device '{}' not found", underlay_dev),
            })?;

    handle
        .link()
        .add(
            LinkVxlan::new(name, vni)
                .dev(parent_index)
                .port(port)
                .learning(false)
                .build(),
        )
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("create: {}", e),
        })?;

    let idx = link_index_by_name(handle, name)
        .await?
        .ok_or(NetworkError::Vxlan {
            device: name.into(),
            reason: "device missing after creation".into(),
        })?;

    handle
        .link()
        .set(LinkUnspec::new_with_index(idx).mtu(mtu).build())
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("set_mtu: {}", e),
        })?;

    handle
        .link()
        .set(LinkUnspec::new_with_index(idx).up().build())
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("link_up: {}", e),
        })?;

    info!(vxlan = %name, vni, port, mtu, parent = %underlay_dev, "vxlan device ready");
    Ok(idx)
}

/// How an existing VXLAN device compares with the one requested.
#[derive(Debug, PartialEq, Eq)]
enum Topology {
    Matches,
    /// A VXLAN device with another parent, VNI or port (the `ip` detail).
    Differs(String),
}

async fn existing_topology(
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
) -> crate::Result<Topology> {
    let output = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", name])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("inspect existing topology: {error}"),
        })?;
    if !output.status.success() {
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "inspect existing topology: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let detail = String::from_utf8_lossy(&output.stdout);
    validate_topology_detail(&detail, underlay_dev, vni, port).map_err(|reason| {
        NetworkError::Vxlan {
            device: name.into(),
            reason,
        }
    })
}

/// `Err` when the device is not VXLAN at all: something else owns the name,
/// and deleting it would be wrong.
fn validate_topology_detail(
    detail: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
) -> std::result::Result<Topology, String> {
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    let Some(vxlan_index) = tokens.iter().position(|token| *token == "vxlan") else {
        return Err(format!("existing device is not VXLAN: {detail}"));
    };
    let topology = &tokens[vxlan_index..];
    let has_pair = |key: &str, expected: &str| {
        topology
            .windows(2)
            .any(|pair| pair[0] == key && pair[1] == expected)
    };
    let expected_vni = vni.to_string();
    let expected_port = port.to_string();
    if !has_pair("id", &expected_vni)
        || !has_pair("dev", underlay_dev)
        || !has_pair("dstport", &expected_port)
    {
        return Ok(Topology::Differs(detail.trim().to_string()));
    }
    Ok(Topology::Matches)
}

/// Enslave the VXLAN device to a bridge so containers on the bridge see it
/// as a regular L2 port.
pub async fn enslave_to_bridge(
    handle: &Handle,
    vxlan_name: &str,
    bridge_name: &str,
) -> crate::Result<()> {
    let vxlan_idx = link_index_by_name(handle, vxlan_name)
        .await?
        .ok_or(NetworkError::Vxlan {
            device: vxlan_name.into(),
            reason: "device not found while enslaving to bridge".into(),
        })?;
    let bridge_idx = link_index_by_name(handle, bridge_name)
        .await?
        .ok_or(NetworkError::Vxlan {
            device: vxlan_name.into(),
            reason: format!("bridge '{}' not found", bridge_name),
        })?;

    handle
        .link()
        .set(
            LinkUnspec::new_with_index(vxlan_idx)
                .controller(bridge_idx)
                .build(),
        )
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: vxlan_name.into(),
            reason: format!("enslave_to_bridge: {}", e),
        })?;
    Ok(())
}

/// Remove a VXLAN device by name. Idempotent.
pub async fn remove(handle: &Handle, name: &str) -> crate::Result<()> {
    let Some(idx) = link_index_by_name(handle, name).await? else {
        return Ok(());
    };
    handle
        .link()
        .del(idx)
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("delete: {}", e),
        })?;
    Ok(())
}

/// Append an FDB entry telling the kernel that broadcast / unknown unicast
/// traffic on `vxlan_dev` should be tunneled to `dst`. We use the all-zero
/// MAC, the standard convention for default-flood entries when learning is
/// disabled.
///
/// We invoke `bridge fdb append` via the `iproute2` toolchain because
/// netlink's FDB API is awkward; `bridge` is universally available.
pub async fn add_fdb(_handle: &Handle, vxlan_dev: &str, dst: IpAddr) -> crate::Result<()> {
    run_bridge(&[
        "fdb",
        "append",
        "00:00:00:00:00:00",
        "dev",
        vxlan_dev,
        "dst",
        &dst.to_string(),
    ])
    .await
    .map_err(|reason| NetworkError::Vxlan {
        device: vxlan_dev.into(),
        reason: format!("add_fdb {}: {}", dst, reason),
    })
}

/// Remove an FDB entry. Idempotent — silently succeeds if the entry is
/// already gone.
pub async fn remove_fdb(_handle: &Handle, vxlan_dev: &str, dst: IpAddr) -> crate::Result<()> {
    let res = run_bridge(&[
        "fdb",
        "delete",
        "00:00:00:00:00:00",
        "dev",
        vxlan_dev,
        "dst",
        &dst.to_string(),
    ])
    .await;

    match res {
        Ok(()) => Ok(()),
        Err(reason) if reason.contains("No such") || reason.contains("Cannot find") => {
            warn!(vxlan = %vxlan_dev, %dst, "fdb entry already removed");
            Ok(())
        }
        Err(reason) => Err(NetworkError::Vxlan {
            device: vxlan_dev.into(),
            reason: format!("remove_fdb {}: {}", dst, reason),
        }),
    }
}

async fn run_bridge(args: &[&str]) -> std::result::Result<(), String> {
    let out = Command::new("bridge")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("spawn bridge: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_topology_detail, Topology};

    const DETAIL: &str = "11: vxlan-temps0: <BROADCAST> mtu 1350 vxlan id 42 dev enp6s0.4000 srcport 0 0 dstport 4789 nolearning";

    #[test]
    fn accepts_matching_existing_vxlan_topology() {
        assert_eq!(
            validate_topology_detail(DETAIL, "enp6s0.4000", 42, 4789),
            Ok(Topology::Matches)
        );
    }

    #[test]
    fn the_device_being_replaced_is_read_for_its_restoration() {
        use super::{parse_previous, Previous};
        let enslaved = "11: vxlan-temps0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1450 qdisc \
                        noqueue master br-temps0 state UNKNOWN mode DEFAULT \\    \
                        link/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff promiscuity 1 \\    \
                        vxlan id 42 dev enp6s0.4000 srcport 0 0 dstport 4789 nolearning \
                        bridge_slave state forwarding";
        assert_eq!(
            parse_previous(enslaved),
            Some(Previous {
                parent: "enp6s0.4000".into(),
                vni: 42,
                port: 4789,
                master: Some("br-temps0".into()),
                fdb: Vec::new(),
            })
        );
        // Not on a bridge yet.
        assert_eq!(
            parse_previous(DETAIL).and_then(|previous| previous.master),
            None
        );
        // Nothing to rebuild a non-VXLAN device from.
        assert_eq!(
            parse_previous("9: dummy0: <BROADCAST> mtu 1500 dummy"),
            None
        );
    }

    #[test]
    fn only_default_flood_entries_are_restored() {
        use super::parse_fdb_destinations;
        let output = "00:00:00:00:00:00 dst 10.0.0.2 self permanent\n\
                      00:00:00:00:00:00 dst fd00::2 self permanent\n\
                      02:42:ac:11:00:02 dst 10.0.0.3 self\n\
                      33:33:00:00:00:01 master br-temps0 permanent\n";
        assert_eq!(
            parse_fdb_destinations(output),
            vec![
                "10.0.0.2".parse::<std::net::IpAddr>().unwrap(),
                "fd00::2".parse().unwrap()
            ]
        );
    }

    #[test]
    fn a_vxlan_on_another_parent_is_to_be_recreated() {
        // The underlay moved, e.g. onto the WireGuard mesh.
        assert!(matches!(
            validate_topology_detail(DETAIL, "temps-wg0", 42, 4789),
            Ok(Topology::Differs(_))
        ));
    }

    #[test]
    fn a_vxlan_with_another_vni_or_port_is_to_be_recreated() {
        assert!(matches!(
            validate_topology_detail(DETAIL, "enp6s0.4000", 99, 4789),
            Ok(Topology::Differs(_))
        ));
        assert!(matches!(
            validate_topology_detail(DETAIL, "enp6s0.4000", 42, 8472),
            Ok(Topology::Differs(_))
        ));
    }

    #[test]
    fn a_device_that_is_not_vxlan_is_an_error() {
        let bridge = "11: vxlan-temps0: <BROADCAST> mtu 1500 bridge forward_delay 1500";
        assert!(validate_topology_detail(bridge, "eth0", 42, 4789).is_err());
    }
}
