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
                return replace(handle, idx, name, underlay_dev, vni, port, mtu).await;
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
    // No device under its name: a swap that stopped after deleting the old
    // device left its replacement staged. Finish it, or clear it so it does
    // not stand in the way of creating the device.
    if let Some(idx) = recover_staged(handle, name, underlay_dev, vni, port, mtu).await? {
        return Ok(idx);
    }
    create(handle, name, underlay_dev, vni, port, mtu).await
}

/// The name a replacement device is built under before it takes the
/// device's own name: at most 15 bytes, the kernel's interface-name limit.
fn staging_name(name: &str) -> String {
    const SUFFIX: &str = "new";
    let keep = name
        .char_indices()
        .map(|(index, ch)| index + ch.len_utf8())
        .take_while(|end| *end <= 15 - SUFFIX.len())
        .last()
        .unwrap_or(0);
    format!("{}{SUFFIX}", &name[..keep])
}

/// Replace the device at `old_index` with one of the requested topology.
/// The replacement is built under [`staging_name`] first: if it cannot be
/// (the new underlay device is missing, the kernel refuses it), the working
/// device is left as it was and its overlay keeps running. Only once the
/// replacement exists is the old device deleted and the replacement renamed
/// into its place.
async fn replace(
    handle: &Handle,
    old_index: u32,
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
    let staging = staging_name(name);
    // A replacement left behind by an interrupted swap is ours to remove;
    // anything else under that name is not, and blocks the swap.
    if let Some(stale) = link_index_by_name(handle, &staging).await? {
        if !staged_by_temps(&staging).await? {
            return Err(NetworkError::Vxlan {
                device: name.into(),
                reason: format!(
                    "interface {staging} exists and was not staged by Temps, so the \
                     replacement for parent={underlay_dev}, vni={vni}, port={port} cannot be \
                     built under that name; the existing device is kept. Rename or remove \
                     {staging}"
                ),
            });
        }
        delete(handle, stale, &staging, "remove a stale replacement").await?;
    }

    let staged = create(handle, &staging, underlay_dev, vni, port, mtu)
        .await
        .map_err(|error| NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "could not build its replacement for parent={underlay_dev}, vni={vni}, \
                 port={port}, so the existing device is kept: {error}"
            ),
        })?;
    if let Err(error) = set_alias(&staging, STAGING_ALIAS).await {
        // Unmarked, a leftover could not be told apart from someone else's.
        if let Err(cleanup) = delete(handle, staged, &staging, "discard replacement").await {
            warn!(vxlan = %staging, error = %cleanup, "could not remove the unused replacement");
        }
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "could not mark its replacement, so the existing device is kept: {error}"
            ),
        });
    }

    if let Err(error) = delete(handle, old_index, name, "delete for replacement").await {
        // Keep the working device; drop the replacement.
        if let Err(cleanup) = delete(handle, staged, &staging, "discard replacement").await {
            warn!(vxlan = %staging, error = %cleanup, "could not remove the unused replacement");
        }
        return Err(error);
    }

    match rename_into_place(handle, staged, name).await {
        Ok(()) => {
            info!(vxlan = %name, vni, port, parent = %underlay_dev, "vxlan device replaced");
            Ok(staged)
        }
        Err(error) => {
            // The old device is gone and the replacement works under the
            // staging name. Build the device under its own name instead,
            // which the staged one just proved possible.
            warn!(vxlan = %name, staging = %staging, %error, "could not rename the replacement; creating it under its own name");
            if let Err(cleanup) = delete(handle, staged, &staging, "discard replacement").await {
                warn!(vxlan = %staging, error = %cleanup, "could not remove the unused replacement");
            }
            create(handle, name, underlay_dev, vni, port, mtu).await
        }
    }
}

/// The alias that marks a device as a replacement Temps staged: what makes a
/// leftover under the staging name Temps' own to finish or remove.
const STAGING_ALIAS: &str = "temps-vxlan-staging";

/// Whether the device called `staging` is a VXLAN replacement Temps staged
/// (it carries [`STAGING_ALIAS`]).
async fn staged_by_temps(staging: &str) -> crate::Result<bool> {
    let output = Command::new("ip")
        .args(["-d", "-o", "link", "show", "dev", staging])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| NetworkError::Vxlan {
            device: staging.into(),
            reason: format!("inspect staged replacement: {error}"),
        })?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(is_staged_detail(&String::from_utf8_lossy(&output.stdout)))
}

/// `ip -d -o link show` detail of a VXLAN device carrying [`STAGING_ALIAS`].
fn is_staged_detail(detail: &str) -> bool {
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    tokens.contains(&"vxlan")
        && tokens
            .windows(2)
            .any(|pair| pair[0] == "alias" && pair[1] == STAGING_ALIAS)
}

async fn set_alias(device: &str, alias: &str) -> Result<(), String> {
    let output = Command::new("ip")
        .args(["link", "set", "dev", device, "alias", alias])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Give the staged device at `staged` the name `name` (a link is renamed
/// while down) and drop its staging mark.
async fn rename_into_place(
    handle: &Handle,
    staged: u32,
    name: &str,
) -> Result<(), rtnetlink::Error> {
    handle
        .link()
        .set(LinkUnspec::new_with_index(staged).down().build())
        .execute()
        .await?;
    handle
        .link()
        .set(LinkUnspec::new_with_index(staged).name(name).build())
        .execute()
        .await?;
    handle
        .link()
        .set(LinkUnspec::new_with_index(staged).up().build())
        .execute()
        .await?;
    if let Err(error) = set_alias(name, "").await {
        debug!(vxlan = %name, %error, "could not clear the staging alias");
    }
    Ok(())
}

/// With no device under `name`: a replacement Temps staged for it, left by a
/// swap that stopped between deleting the old device and renaming the new
/// one. One built for the requested topology takes the name; any other is
/// removed so it cannot block creating the device. An interface under the
/// staging name that Temps did not stage is left alone.
async fn recover_staged(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<Option<u32>> {
    let staging = staging_name(name);
    let Some(staged) = link_index_by_name(handle, &staging).await? else {
        return Ok(None);
    };
    if !staged_by_temps(&staging).await? {
        warn!(vxlan = %name, staging = %staging, "an interface under the staging name was not staged by Temps; leaving it alone");
        return Ok(None);
    }
    match existing_topology(&staging, underlay_dev, vni, port).await? {
        Topology::Matches => {
            rename_into_place(handle, staged, name)
                .await
                .map_err(|e| NetworkError::Vxlan {
                    device: name.into(),
                    reason: format!("finish an interrupted replacement from {staging}: {e}"),
                })?;
            handle
                .link()
                .set(LinkUnspec::new_with_index(staged).mtu(mtu).build())
                .execute()
                .await
                .map_err(|e| NetworkError::Vxlan {
                    device: name.into(),
                    reason: format!("set_mtu: {e}"),
                })?;
            info!(vxlan = %name, staging = %staging, "finished an interrupted vxlan replacement");
            Ok(Some(staged))
        }
        Topology::Differs(detail) => {
            warn!(vxlan = %name, staging = %staging, existing = %detail, "removing a staged replacement built for another topology");
            delete(handle, staged, &staging, "remove a stale replacement").await?;
            Ok(None)
        }
    }
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
    fn only_a_marked_vxlan_counts_as_staged_by_temps() {
        use super::is_staged_detail;
        let staged = "9: vxlan-temps0new: <BROADCAST> mtu 1450 qdisc noop state DOWN \\    \
                      link/ether 02:00:00:00:00:01 brd ff:ff:ff:ff:ff:ff promiscuity 0 \\    \
                      vxlan id 42 dev temps-wg0 srcport 0 0 dstport 4789 nolearning \\    \
                      alias temps-vxlan-staging";
        assert!(is_staged_detail(staged));
        // Someone else's VXLAN under the name, without the mark.
        assert!(!is_staged_detail(
            &staged.replace("alias temps-vxlan-staging", "")
        ));
        // Not a VXLAN device at all, even if it carries the alias.
        assert!(!is_staged_detail(
            "9: vxlan-temps0new: <BROADCAST> mtu 1500 \\ dummy \\ alias temps-vxlan-staging"
        ));
    }

    #[test]
    fn the_replacement_is_staged_under_a_valid_interface_name() {
        use super::staging_name;
        assert_eq!(staging_name("vxlan-temps0"), "vxlan-temps0new");
        // Kernel interface names are at most 15 bytes.
        let long = staging_name("vxlan-temps-cluster0");
        assert_eq!(long, "vxlan-temps-new");
        assert!(long.len() <= 15);
        assert_ne!(staging_name("vxlan-temps0"), "vxlan-temps0");
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
