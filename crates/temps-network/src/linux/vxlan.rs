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
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
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
/// can be checked beforehand is (the new parent exists, no other VXLAN device
/// holds the VNI and port); if the kernel still refuses the replacement, the
/// previous device is rebuilt as it was — same topology, MTU, bridge and FDB —
/// so the overlay keeps running, and the refusal is remembered (see
/// [`refusal_backoff`]) so retries in the meantime keep that device instead of
/// tearing it down to hit the same refusal. A process that stops between the
/// two steps leaves no device under the name, and the next bootstrap simply
/// creates it.
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
    if let Some(holder) = vxlan_holding(name, vni, port).await? {
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "VXLAN device '{holder}' already holds vni={vni}, port={port}, so no \
                 replacement on parent={underlay_dev} can be built; the existing device is kept"
            ),
        });
    }
    let request = Replacement {
        name: name.into(),
        parent: underlay_dev.into(),
        vni,
        port,
        mtu,
    };
    let parent = parent_state(underlay_dev).await;
    if let Some((reason, retry_in)) = recent_refusal(&request, parent.as_deref()) {
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "the kernel refused its replacement for parent={underlay_dev}, vni={vni}, \
                 port={port} ({reason}); the existing device is kept and the replacement is \
                 retried in {}s",
                retry_in.as_secs()
            ),
        });
    }
    let previous = Previous::capture(name, previous_detail).await;

    delete(handle, old_index, name, "delete for replacement").await?;
    match create(handle, name, underlay_dev, vni, port, mtu).await {
        Ok(index) => {
            forget_refusal(&request);
            info!(vxlan = %name, vni, port, parent = %underlay_dev, "vxlan device replaced");
            Ok(index)
        }
        Err(error) => {
            remember_refusal(request, parent, error.to_string());
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

/// How long a replacement the kernel refused `attempts` times in a row is not
/// attempted again, for the same device, requested topology and state of the
/// new parent. Each attempt deletes the working device first, so retrying on
/// every reconcile would keep interrupting the overlay to hit the same
/// refusal; a short first wait, doubling up to ten minutes, recovers quickly
/// from a passing failure without doing that. A repair of the parent itself
/// (recreated, brought up, a new MTU) cuts the wait to
/// [`REPAIRED_PARENT_RETRY`].
fn refusal_backoff(attempts: u32) -> Duration {
    const FIRST: Duration = Duration::from_secs(30);
    const MAX: Duration = Duration::from_secs(10 * 60);
    FIRST
        .checked_mul(1 << attempts.saturating_sub(1).min(5))
        .map_or(MAX, |backoff| backoff.min(MAX))
}

/// The least time between two attempts at a replacement, even once its parent
/// was repaired: each attempt interrupts the overlay, and the agent re-runs
/// bootstrap every few seconds after a failure, so a parent that keeps
/// changing must not turn into an attempt per run.
const REPAIRED_PARENT_RETRY: Duration = Duration::from_secs(15);

/// A requested replacement: the device and the topology asked of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    name: String,
    parent: String,
    vni: u32,
    port: u16,
    mtu: u32,
}

struct Refusal {
    request: Replacement,
    /// [`parent_state`] of the new parent when it was refused.
    parent: Option<String>,
    reason: String,
    attempts: u32,
    at: Instant,
}

/// The last replacement the kernel refused. Bootstrap is control-plane work
/// (startup and reconciles), so a lock is fine here.
static LAST_REFUSAL: Mutex<Option<Refusal>> = Mutex::new(None);

fn last_refusal() -> MutexGuard<'static, Option<Refusal>> {
    LAST_REFUSAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The reason `request` was refused and how long until it may be retried:
/// its backoff while the new parent is in the state it was refused in, and
/// [`REPAIRED_PARENT_RETRY`] once that changed.
fn recent_refusal(request: &Replacement, parent: Option<&str>) -> Option<(String, Duration)> {
    let last = last_refusal();
    let refusal = last
        .as_ref()
        .filter(|refusal| refusal.request == *request)?;
    let wait = if refusal.parent.as_deref() == parent {
        refusal_backoff(refusal.attempts)
    } else {
        REPAIRED_PARENT_RETRY
    };
    let retry_in = wait.checked_sub(refusal.at.elapsed())?;
    Some((refusal.reason.clone(), retry_in))
}

fn remember_refusal(request: Replacement, parent: Option<String>, reason: String) {
    let mut last = last_refusal();
    // Counted per request, whatever the parent's state: a parent that keeps
    // changing without the replacement ever succeeding still backs off.
    let attempts = match last.as_ref() {
        Some(refusal) if refusal.request == request => refusal.attempts.saturating_add(1),
        _ => 1,
    };
    *last = Some(Refusal {
        request,
        parent,
        reason,
        attempts,
        at: Instant::now(),
    });
}

/// What about the parent device a repair would change — its index (it was
/// recreated), whether it is administratively up and its MTU — or `None` if
/// it cannot be read. Carrier (`LOWER_UP`) is left out: it flaps on its own,
/// and a flap repairs nothing.
async fn parent_state(device: &str) -> Option<String> {
    let output = Command::new("ip")
        .args(["-o", "link", "show", "dev", device])
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    link_state(&String::from_utf8_lossy(&output.stdout))
}

/// [`parent_state`] from an `ip -o link show` line.
fn link_state(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let index = tokens.first()?.trim_end_matches(':');
    let flags = tokens.iter().find(|token| token.starts_with('<'))?;
    let admin_up = flags
        .trim_matches(|c| c == '<' || c == '>')
        .split(',')
        .any(|flag| flag == "UP");
    let mtu = tokens.windows(2).find(|pair| pair[0] == "mtu")?[1];
    Some(format!(
        "{index} {} mtu {mtu}",
        if admin_up { "up" } else { "down" }
    ))
}

fn forget_refusal(request: &Replacement) {
    let mut last = last_refusal();
    if last
        .as_ref()
        .is_some_and(|refusal| refusal.request == *request)
    {
        *last = None;
    }
}

/// Another VXLAN device (not `name`) holding `vni` on `port`: the kernel
/// refuses a second one, so a replacement could not be created.
async fn vxlan_holding(name: &str, vni: u32, port: u16) -> crate::Result<Option<String>> {
    let output = Command::new("ip")
        .args(["-d", "-o", "link", "show", "type", "vxlan"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("list VXLAN devices: {error}"),
        })?;
    if !output.status.success() {
        return Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!(
                "list VXLAN devices: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(holder_of(
        &String::from_utf8_lossy(&output.stdout),
        name,
        vni,
        port,
    ))
}

/// From `ip -d -o link show type vxlan` output: the device other than `name`
/// with `vni` on `port`.
fn holder_of(output: &str, name: &str, vni: u32, port: u16) -> Option<String> {
    let (vni, port) = (vni.to_string(), port.to_string());
    output.lines().find_map(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // "11: vxlan-temps0: <...>" or "11: vx0@eth0: <...>".
        let device = tokens.get(1)?.trim_end_matches(':');
        let device = device.split('@').next().unwrap_or(device);
        let topology = &tokens[tokens.iter().position(|token| *token == "vxlan")?..];
        let has_pair = |key: &str, value: &str| {
            topology
                .windows(2)
                .any(|pair| pair[0] == key && pair[1] == value)
        };
        (device != name && has_pair("id", &vni) && has_pair("dstport", &port))
            .then(|| device.to_string())
    })
}

/// A device about to be replaced, as needed to rebuild it.
#[derive(Debug, PartialEq, Eq)]
struct Previous {
    parent: String,
    vni: u32,
    port: u16,
    mtu: Option<u32>,
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

    /// Rebuild the device. `requested_mtu` is only a fallback: the previous
    /// parent may not carry the MTU asked of the replacement.
    async fn restore(&self, handle: &Handle, name: &str, requested_mtu: u32) -> Result<(), String> {
        let mtu = self.mtu.unwrap_or(requested_mtu);
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

/// The parent, VNI, port, MTU and bridge of a VXLAN device from its
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
        mtu: value(link, "mtu").and_then(|mtu| mtu.parse().ok()),
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

    // A device that cannot take its MTU or come up is not left behind: it
    // would hold the name (and the VNI) that a retry, or restoring the
    // device it replaced, needs.
    if let Err(error) = bring_up(handle, idx, name, mtu).await {
        if let Err(cleanup) = delete(handle, idx, name, "remove an unfinished device").await {
            warn!(vxlan = %name, error = %cleanup, "could not remove an unfinished vxlan device");
        }
        return Err(error);
    }

    info!(vxlan = %name, vni, port, mtu, parent = %underlay_dev, "vxlan device ready");
    Ok(idx)
}

async fn bring_up(handle: &Handle, idx: u32, name: &str, mtu: u32) -> crate::Result<()> {
    handle
        .link()
        .set(LinkUnspec::new_with_index(idx).mtu(mtu).build())
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("set_mtu {mtu}: {e}"),
        })?;
    handle
        .link()
        .set(LinkUnspec::new_with_index(idx).up().build())
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("link_up: {e}"),
        })
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
                mtu: Some(1450),
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
    fn another_device_holding_the_vni_and_port_is_found() {
        use super::holder_of;
        let output = "11: vxlan-temps0: <UP> mtu 1450 master br-temps0 \\    vxlan id 42 \
                      dev eth0 srcport 0 0 dstport 4789 nolearning\n\
                      12: vx0@dummy0: <UP> mtu 1450 \\    vxlan id 42 dev dummy0 srcport 0 0 \
                      dstport 4789 nolearning\n\
                      13: vx1: <UP> mtu 1450 \\    vxlan id 7 dev eth0 dstport 4789\n";
        assert_eq!(
            holder_of(output, "vxlan-temps0", 42, 4789).as_deref(),
            Some("vx0")
        );
        // The device being replaced does not block its own replacement.
        assert_eq!(holder_of(output, "vx0", 7, 4789).as_deref(), Some("vx1"));
        assert_eq!(holder_of(output, "vxlan-temps0", 42, 4790), None);
        assert_eq!(holder_of(output, "vx1", 7, 4789), None);
    }

    #[test]
    fn a_refused_replacement_is_not_retried_until_the_backoff_passes() {
        use super::{forget_refusal, recent_refusal, remember_refusal, Replacement};
        use std::time::Duration;
        let request = Replacement {
            name: "vxlan-unit0".into(),
            parent: "temps-wg0".into(),
            vni: 42,
            port: 4789,
            mtu: 1370,
        };
        let down = Some("7 down mtu 1420");
        remember_refusal(
            request.clone(),
            down.map(Into::into),
            "set_mtu 1370: invalid argument".into(),
        );
        let (reason, retry_in) = recent_refusal(&request, down).expect("refused just now");
        assert!(reason.contains("invalid argument"));
        assert!(retry_in <= Duration::from_secs(30), "{retry_in:?}");
        // Refused again in the same state: the wait grows.
        remember_refusal(request.clone(), down.map(Into::into), "again".into());
        let (_, retry_in) = recent_refusal(&request, down).expect("refused twice");
        assert!(retry_in > Duration::from_secs(30), "{retry_in:?}");

        // The parent was repaired (here: brought up): the wait drops to the
        // floor between attempts, not to nothing.
        let (_, retry_in) =
            recent_refusal(&request, Some("7 up mtu 1420")).expect("too soon after the attempt");
        assert!(retry_in <= super::REPAIRED_PARENT_RETRY, "{retry_in:?}");
        // As is another request, e.g. a corrected MTU.
        assert!(recent_refusal(
            &Replacement {
                mtu: 1320,
                ..request.clone()
            },
            down
        )
        .is_none());
        // A refusal in the repaired state keeps counting the attempts.
        remember_refusal(
            request.clone(),
            Some("7 up mtu 1420".into()),
            "still".into(),
        );
        let (_, retry_in) =
            recent_refusal(&request, Some("7 up mtu 1420")).expect("refused three times");
        assert!(retry_in > Duration::from_secs(60), "{retry_in:?}");
        forget_refusal(&request);
        assert!(recent_refusal(&request, down).is_none());
    }

    #[test]
    fn the_backoff_starts_short_and_is_capped() {
        use super::refusal_backoff;
        use std::time::Duration;
        assert_eq!(refusal_backoff(1), Duration::from_secs(30));
        assert_eq!(refusal_backoff(2), Duration::from_secs(60));
        assert_eq!(refusal_backoff(5), Duration::from_secs(480));
        assert_eq!(refusal_backoff(6), Duration::from_secs(600));
        assert_eq!(refusal_backoff(u32::MAX), Duration::from_secs(600));
    }

    #[test]
    fn the_parent_state_tracks_what_a_repair_changes() {
        use super::link_state;
        let down = "7: temps-wg0: <POINTOPOINT,NOARP> mtu 1420 qdisc noop state DOWN \
                    mode DEFAULT group default qlen 1000\\    link/none ";
        let up = "7: temps-wg0: <POINTOPOINT,NOARP,UP,LOWER_UP> mtu 1420 qdisc noqueue \
                  state UNKNOWN mode DEFAULT group default qlen 1000\\    link/none ";
        assert_eq!(link_state(down).as_deref(), Some("7 down mtu 1420"));
        assert_eq!(link_state(up).as_deref(), Some("7 up mtu 1420"));
        // A carrier flap is not a repair.
        assert_eq!(
            link_state(up),
            link_state(&up.replace(",UP,LOWER_UP>", ",UP>"))
        );
        // `LOWER_UP` alone does not read as administratively up.
        assert_eq!(
            link_state(&down.replace("NOARP>", "NOARP,LOWER_UP>")).as_deref(),
            Some("7 down mtu 1420")
        );
        assert_ne!(
            link_state(up),
            link_state(&up.replace("mtu 1420", "mtu 1500"))
        );
        assert_ne!(link_state(up), link_state(&up.replacen("7:", "9:", 1)));
        assert_eq!(link_state("garbage"), None);
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
        // `dev enp6s0` must not match `dev enp6s0.4000` as a prefix.
        assert!(matches!(
            validate_topology_detail(DETAIL, "enp6s0", 42, 4789),
            Ok(Topology::Differs(_))
        ));
    }

    #[test]
    fn a_vxlan_without_a_parent_is_to_be_recreated() {
        let detail = "11: vxlan-temps0: <BROADCAST> mtu 1450 vxlan id 42 srcport 0 0 dstport 4789 nolearning";
        assert!(matches!(
            validate_topology_detail(detail, "eth0", 42, 4789),
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
