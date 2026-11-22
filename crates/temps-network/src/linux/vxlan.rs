// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! VXLAN device + FDB management.
//!
//! Devices are created and read back through rtnetlink (see
//! [`crate::linux::inspect`] for why they are not read from `ip -d` output).
//! FDB entries (`bridge fdb append`)
//! are managed via the `bridge` command-line tool because rtnetlink's FDB
//! support is awkward and `bridge` is part of `iproute2` which is installed
//! on every Linux distribution we care about.

use crate::error::NetworkError;
use crate::linux::bridge::{link_index_by_name, link_name_by_index};
use crate::linux::inspect::{command_failure, LinkDescription};
use futures::TryStreamExt;
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
///
/// `local` is this node's underlay address. It is the source of every
/// encapsulated packet, so peers (whose firewall admits VXLAN only from known
/// underlay addresses) always see the address they know us by. It does NOT
/// narrow what the device receives: the kernel binds the VXLAN UDP socket to
/// the wildcard address whatever `local` is, and decapsulates a packet sent
/// to any of the host's addresses. The ingress allowlist in
/// [`crate::linux::firewall`] is what restricts VXLAN to known peers.
pub async fn ensure(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    local: IpAddr,
    vni: u32,
    port: u16,
    mtu: u32,
) -> crate::Result<u32> {
    if let Some(existing) = read_link(handle, name).await? {
        let idx = existing.index;
        debug!(vxlan = %name, idx, "vxlan device already exists");
        match existing_topology(handle, existing, underlay_dev, local, vni, port).await? {
            Topology::Matches => {}
            Topology::LocalDiffers(existing) => {
                // Only the local address is off — every device created before
                // it was set. The kernel changes it in place, keeping the
                // device's index, bridge port and FDB, so the overlay is not
                // interrupted. It refuses a change of address family; only
                // then is the device rebuilt.
                if let Err(error) =
                    set_local_in_place(handle, idx, name, underlay_dev, local, vni, port).await
                {
                    warn!(vxlan = %name, %local, existing = %existing, %error, "could not set the vxlan local address in place; recreating the device");
                    return replace(handle, &existing, name, underlay_dev, local, vni, port, mtu)
                        .await;
                }
                info!(vxlan = %name, %local, "vxlan local address set in place");
            }
            Topology::Differs(existing) => {
                // Our device, built for another parent, VNI or port — e.g. the
                // underlay moved onto the WireGuard mesh. None of these can be
                // changed in place, so replace it; bootstrap re-enslaves it
                // and repopulates the FDB right after.
                warn!(vxlan = %name, parent = %underlay_dev, %local, vni, port, existing = %existing, "recreating vxlan device for a changed topology");
                return replace(handle, &existing, name, underlay_dev, local, vni, port, mtu).await;
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
    create(handle, name, underlay_dev, Some(local), vni, port, mtu).await
}

/// [`set_local`], then read the device back: a request the kernel
/// acknowledges without applying must not pass for a fixed device.
async fn set_local_in_place(
    handle: &Handle,
    index: u32,
    name: &str,
    underlay_dev: &str,
    local: IpAddr,
    vni: u32,
    port: u16,
) -> crate::Result<()> {
    set_local(handle, index, name, vni, local).await?;
    let after = read_link(handle, name)
        .await?
        .ok_or_else(|| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("set local {local} was acknowledged but the device is gone"),
        })?;
    match existing_topology(handle, after, underlay_dev, local, vni, port).await? {
        Topology::Matches => Ok(()),
        Topology::LocalDiffers(after) | Topology::Differs(after) => Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!("set local {local} was acknowledged but not applied: {after}"),
        }),
    }
}

/// Set the local address of the existing device at `index` without
/// recreating it. This has to be an `RTM_NEWLINK` on the existing device (as
/// `ip link set ... type vxlan` sends): `RTM_SETLINK` ignores link-type
/// attributes. Only the VNI (unchanged, which the kernel requires to be sent
/// as is) and the address are sent; the kernel keeps every other attribute of
/// the device.
async fn set_local(
    handle: &Handle,
    index: u32,
    name: &str,
    vni: u32,
    local: IpAddr,
) -> crate::Result<()> {
    let builder = LinkVxlan::new(name, vni).index(index);
    let builder = match local {
        IpAddr::V4(local) => builder.local(local),
        IpAddr::V6(local) => builder.local6(local),
    };
    handle
        .link()
        .change(builder.build())
        .execute()
        .await
        .map_err(|e| NetworkError::Vxlan {
            device: name.into(),
            reason: format!("set local {local}: {e}"),
        })
}

/// Replace the `existing` device with one of the requested topology.
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
    existing: &LinkDescription,
    name: &str,
    underlay_dev: &str,
    local: IpAddr,
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
    if let Some(holder) = vxlan_holding(handle, name, vni, port).await? {
        // The existing device is kept: nothing was changed yet.
        return Err(id_in_use(name, holder, vni, port).await);
    }
    let request = Replacement {
        name: name.into(),
        parent: underlay_dev.into(),
        local,
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
    let previous = Previous::capture(handle, name, existing).await;

    delete(handle, existing.index, name, "delete for replacement").await?;
    match create(handle, name, underlay_dev, Some(local), vni, port, mtu).await {
        Ok(index) => {
            forget_refusal(&request);
            info!(vxlan = %name, vni, port, parent = %underlay_dev, %local, "vxlan device replaced");
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
    local: IpAddr,
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

/// `holder` keeps `name` from being created on `vni`/`port`: say what it
/// belongs to and what to do (ADR 062).
async fn id_in_use(name: &str, holder: String, vni: u32, port: u16) -> NetworkError {
    let diagnosis = crate::vxlan_holder::diagnose(&holder, name, vni, port).await;
    NetworkError::VxlanIdInUse {
        device: name.into(),
        holder,
        vni,
        port,
        diagnosis,
    }
}

/// Another VXLAN device (not `name`) holding `vni` on `port`: the kernel
/// refuses a second one, so neither a new device nor a replacement could be
/// created.
async fn vxlan_holding(
    handle: &Handle,
    name: &str,
    vni: u32,
    port: u16,
) -> crate::Result<Option<String>> {
    let list_failed = |error: rtnetlink::Error| NetworkError::Vxlan {
        device: name.into(),
        reason: format!(
            "list links for another VXLAN device holding vni={vni}, port={port}: {error}"
        ),
    };
    let mut links = handle.link().get().execute();
    let mut holder = None;
    // The dump is read to its end rather than dropped part-way.
    while let Some(message) = links.try_next().await.map_err(list_failed)? {
        let link = LinkDescription::from_message(&message);
        if holder.is_none() && link.holds(name, vni, port) {
            holder = Some(link.label());
        }
    }
    Ok(holder)
}

/// The link named `name` as the kernel reports it, or `None` if there is
/// none.
async fn read_link(handle: &Handle, name: &str) -> crate::Result<Option<LinkDescription>> {
    let mut links = handle.link().get().match_name(name).execute();
    match links.try_next().await {
        Ok(message) => Ok(message.map(|message| LinkDescription::from_message(&message))),
        Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -libc::ENODEV => Ok(None),
        Err(error) => Err(NetworkError::Vxlan {
            device: name.into(),
            reason: format!("inspect existing topology: {error}"),
        }),
    }
}

/// A device about to be replaced, as needed to rebuild it.
#[derive(Debug, PartialEq, Eq)]
struct Previous {
    parent: String,
    /// `None` for a device created before the local address was set.
    local: Option<IpAddr>,
    vni: u32,
    port: u16,
    mtu: Option<u32>,
    master: Option<String>,
    fdb: Vec<IpAddr>,
}

impl Previous {
    async fn capture(handle: &Handle, name: &str, existing: &LinkDescription) -> Option<Self> {
        let mut previous = match Self::of(handle, existing).await {
            Ok(previous) => previous,
            Err(reason) => {
                warn!(vxlan = %name, %existing, %reason, "could not read the topology of the device being replaced; it cannot be restored if its replacement fails");
                return None;
            }
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
                warn!(vxlan = %name, error = %command_failure(&output), "could not read the FDB of the device being replaced");
            }
            Err(error) => {
                warn!(vxlan = %name, %error, "could not read the FDB of the device being replaced");
            }
        }
        Some(previous)
    }

    /// The parent, local address, VNI, port, MTU and bridge of `existing`,
    /// with the parent
    /// and bridge resolved to the names a rebuild takes. The FDB is read
    /// separately.
    async fn of(handle: &Handle, existing: &LinkDescription) -> Result<Self, String> {
        let vxlan = existing.vxlan.ok_or("it is not a VXLAN device")?;
        let (Some(vni), Some(port)) = (vxlan.vni, vxlan.port) else {
            return Err("the kernel reported no VNI or port for it".into());
        };
        let parent_index = vxlan.parent.ok_or("it has no parent device")?;
        let parent = link_name_by_index(handle, parent_index)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("its parent device (index {parent_index}) no longer exists"))?;
        let master = match existing.controller {
            Some(index) => Some(
                link_name_by_index(handle, index)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("its bridge (index {index}) no longer exists"))?,
            ),
            None => None,
        };
        Ok(Self {
            parent,
            local: vxlan.local,
            vni,
            port,
            mtu: existing.mtu,
            master,
            fdb: Vec::new(),
        })
    }

    /// Rebuild the device. `requested_mtu` is only a fallback: the previous
    /// parent may not carry the MTU asked of the replacement.
    async fn restore(&self, handle: &Handle, name: &str, requested_mtu: u32) -> Result<(), String> {
        let mtu = self.mtu.unwrap_or(requested_mtu);
        create(
            handle,
            name,
            &self.parent,
            self.local,
            self.vni,
            self.port,
            mtu,
        )
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

/// Create the device. `local` is `None` only when restoring a device that was
/// built without one; every new device gets the node's underlay address.
async fn create(
    handle: &Handle,
    name: &str,
    underlay_dev: &str,
    local: Option<IpAddr>,
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

    // The kernel refuses a second device on the same VNI and port with a
    // bare EEXIST. The usual holder is another cluster's overlay left on
    // this host under another name (ADR 059): name it, since every node of
    // a cluster must share the VNI and port and only the holder can move.
    if let Some(holder) = vxlan_holding(handle, name, vni, port).await? {
        return Err(id_in_use(name, holder, vni, port).await);
    }

    let builder = LinkVxlan::new(name, vni)
        .dev(parent_index)
        .port(port)
        .learning(false);
    let builder = match local {
        Some(IpAddr::V4(local)) => builder.local(local),
        Some(IpAddr::V6(local)) => builder.local6(local),
        None => builder,
    };
    handle
        .link()
        .add(builder.build())
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

    info!(vxlan = %name, vni, port, mtu, parent = %underlay_dev, local = ?local, "vxlan device ready");
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
    /// A VXLAN device whose parent, VNI and port match but whose local
    /// address is missing or another one: fixable in place.
    LocalDiffers(LinkDescription),
    /// A VXLAN device with another parent, VNI or port.
    Differs(LinkDescription),
}

/// Compare `existing` with the requested topology. `Err` when it is not a
/// VXLAN device at all: something else owns the name, and deleting it would
/// be wrong.
async fn existing_topology(
    handle: &Handle,
    existing: LinkDescription,
    underlay_dev: &str,
    local: IpAddr,
    vni: u32,
    port: u16,
) -> crate::Result<Topology> {
    let parent = link_index_by_name(handle, underlay_dev).await?;
    match existing.matches_topology(parent, vni, port) {
        // A device without `local` — every one created before it was set —
        // gets it on upgrade.
        Ok(true) if existing.has_local(local) => Ok(Topology::Matches),
        Ok(true) => Ok(Topology::LocalDiffers(existing)),
        Ok(false) => Ok(Topology::Differs(existing)),
        Err(reason) => Err(NetworkError::Vxlan {
            device: existing.label(),
            reason,
        }),
    }
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
        Err(command_failure(&out))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_refused_replacement_is_not_retried_until_the_backoff_passes() {
        use super::{forget_refusal, recent_refusal, remember_refusal, Replacement};
        use std::time::Duration;
        let request = Replacement {
            name: "vxlan-unit0".into(),
            parent: "temps-wg0".into(),
            local: std::net::IpAddr::from([10, 201, 0, 2]),
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
}
