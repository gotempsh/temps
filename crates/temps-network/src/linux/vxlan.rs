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
///
/// An existing device with the requested VNI and port but a different parent
/// is this overlay on an old underlay (for example, a node that used to take
/// the default-route device and now uses its private-address device), so it
/// is deleted and recreated on `underlay_dev`. The caller re-enslaves it and
/// re-adds peer FDB entries right after. Any other mismatch is refused, and
/// so is a missing `underlay_dev`: the working overlay is only deleted once
/// its replacement's parent is known to exist.
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
        match inspect_existing_topology(name, underlay_dev, vni, port).await? {
            ExistingTopology::Compatible => {}
            ExistingTopology::OtherParent { detail } => {
                let parent_index = underlay_index(handle, name, underlay_dev).await?;
                warn!(
                    vxlan = %name,
                    parent = %underlay_dev,
                    vni,
                    port,
                    existing = %detail.trim(),
                    "existing vxlan device is on a different underlay device; recreating it"
                );
                handle
                    .link()
                    .del(idx)
                    .execute()
                    .await
                    .map_err(|e| NetworkError::Vxlan {
                        device: name.into(),
                        reason: format!(
                            "delete before moving to underlay device '{underlay_dev}': {e}"
                        ),
                    })?;
                return create(handle, name, underlay_dev, parent_index, vni, port, mtu).await;
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

    let parent_index = underlay_index(handle, name, underlay_dev).await?;
    create(handle, name, underlay_dev, parent_index, vni, port, mtu).await
}

async fn underlay_index(handle: &Handle, name: &str, underlay_dev: &str) -> crate::Result<u32> {
    link_index_by_name(handle, underlay_dev)
        .await?
        .ok_or(NetworkError::Vxlan {
            device: name.into(),
            reason: format!("underlay device '{}' not found", underlay_dev),
        })
}

async fn create(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    parent_index: u32,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
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

/// How an existing device compares with the requested VXLAN topology.
#[derive(Debug, PartialEq, Eq)]
enum ExistingTopology {
    Compatible,
    /// Same VNI and port, different (or no) parent device.
    OtherParent {
        detail: String,
    },
}

async fn inspect_existing_topology(
    name: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
) -> crate::Result<ExistingTopology> {
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
    classify_topology_detail(&detail, underlay_dev, vni, port).map_err(|reason| {
        NetworkError::Vxlan {
            device: name.into(),
            reason,
        }
    })
}

fn classify_topology_detail(
    detail: &str,
    underlay_dev: &str,
    vni: u32,
    port: u16,
) -> std::result::Result<ExistingTopology, String> {
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
    if !has_pair("id", &expected_vni) || !has_pair("dstport", &expected_port) {
        return Err(format!(
            "existing VXLAN topology does not match requested parent={underlay_dev}, vni={vni}, port={port}: {detail}"
        ));
    }
    if !has_pair("dev", underlay_dev) {
        return Ok(ExistingTopology::OtherParent {
            detail: detail.to_owned(),
        });
    }
    Ok(ExistingTopology::Compatible)
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
    use super::{classify_topology_detail, ExistingTopology};

    const DETAIL: &str = "11: vxlan-temps0: <BROADCAST> mtu 1350 vxlan id 42 dev enp6s0.4000 srcport 0 0 dstport 4789 nolearning";

    #[test]
    fn accepts_matching_existing_vxlan_topology() {
        assert_eq!(
            classify_topology_detail(DETAIL, "enp6s0.4000", 42, 4789),
            Ok(ExistingTopology::Compatible)
        );
    }

    #[test]
    fn existing_vxlan_on_another_parent_is_marked_for_recreation() {
        assert_eq!(
            classify_topology_detail(DETAIL, "wg0", 42, 4789),
            Ok(ExistingTopology::OtherParent {
                detail: DETAIL.to_owned()
            })
        );
        // `dev enp6s0` must not match `dev enp6s0.4000` as a prefix.
        assert!(matches!(
            classify_topology_detail(DETAIL, "enp6s0", 42, 4789),
            Ok(ExistingTopology::OtherParent { .. })
        ));
    }

    #[test]
    fn existing_vxlan_without_a_parent_is_marked_for_recreation() {
        let detail = "11: vxlan-temps0: <BROADCAST> mtu 1450 vxlan id 42 srcport 0 0 dstport 4789 nolearning";
        assert!(matches!(
            classify_topology_detail(detail, "eth0", 42, 4789),
            Ok(ExistingTopology::OtherParent { .. })
        ));
    }

    #[test]
    fn rejects_existing_vxlan_with_wrong_vni_or_port() {
        let error = classify_topology_detail(DETAIL, "enp6s0.4000", 99, 4789).unwrap_err();
        assert!(error.contains("vni=99"));
        assert!(classify_topology_detail(DETAIL, "enp6s0.4000", 42, 8472).is_err());
        // A different parent does not excuse a different VNI.
        assert!(classify_topology_detail(DETAIL, "wg0", 99, 4789).is_err());
    }

    #[test]
    fn rejects_a_device_that_is_not_vxlan() {
        let detail = "11: vxlan-temps0: <BROADCAST> mtu 1500 bridge forward_delay 1500";
        assert!(classify_topology_detail(detail, "eth0", 42, 4789)
            .unwrap_err()
            .contains("not VXLAN"));
    }
}
