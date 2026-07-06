//! Linux bridge management via rtnetlink.

use crate::config::{NetworkConfig, NodeAlloc};
use crate::error::NetworkError;
use futures::TryStreamExt;
use rtnetlink::{Handle, LinkBridge, LinkUnspec};
use std::net::IpAddr;
use tracing::{debug, info};

/// Ensure that a bridge with the given name exists, is up, has the requested
/// address, and has the configured MTU. Idempotent.
pub async fn ensure(
    handle: &Handle,
    name: &str,
    alloc: &NodeAlloc,
    config: &NetworkConfig,
) -> crate::Result<u32> {
    let mtu = config.transport.bridge_mtu(config.underlay_mtu);

    let existing = link_index_by_name(handle, name).await?;
    let index = match existing {
        Some(idx) => {
            debug!(bridge = %name, idx, "bridge already exists");
            idx
        }
        None => {
            handle
                .link()
                .add(LinkBridge::new(name).build())
                .execute()
                .await
                .map_err(|e| NetworkError::Netlink {
                    op: "create_bridge",
                    link: name.into(),
                    reason: e.to_string(),
                })?;
            link_index_by_name(handle, name)
                .await?
                .ok_or(NetworkError::Netlink {
                    op: "create_bridge",
                    link: name.into(),
                    reason: "bridge missing after creation".into(),
                })?
        }
    };

    // Set MTU. Idempotent — the kernel accepts setting the same value.
    handle
        .link()
        .set(LinkUnspec::new_with_index(index).mtu(mtu).build())
        .execute()
        .await
        .map_err(|e| NetworkError::Netlink {
            op: "set_mtu",
            link: name.into(),
            reason: e.to_string(),
        })?;

    // Address.
    let prefix_len = alloc.compute_cidr.prefix_len();
    ensure_addr(handle, index, alloc.bridge_address, prefix_len, name).await?;

    // Bring up.
    handle
        .link()
        .set(LinkUnspec::new_with_index(index).up().build())
        .execute()
        .await
        .map_err(|e| NetworkError::Netlink {
            op: "link_up",
            link: name.into(),
            reason: e.to_string(),
        })?;

    info!(bridge = %name, mtu, addr = %alloc.bridge_address, "bridge ready");
    Ok(index)
}

/// Remove a bridge by name. Returns `Ok(())` when the bridge is already gone.
pub async fn remove(handle: &Handle, name: &str) -> crate::Result<()> {
    let Some(index) = link_index_by_name(handle, name).await? else {
        return Ok(());
    };
    handle
        .link()
        .del(index)
        .execute()
        .await
        .map_err(|e| NetworkError::Netlink {
            op: "delete_bridge",
            link: name.into(),
            reason: e.to_string(),
        })?;
    Ok(())
}

/// Look up a link's interface index by name. Returns `Ok(None)` when the
/// link does not exist.
pub async fn link_index_by_name(handle: &Handle, name: &str) -> crate::Result<Option<u32>> {
    let mut links = handle.link().get().match_name(name.into()).execute();
    match links.try_next().await {
        Ok(Some(msg)) => Ok(Some(msg.header.index)),
        Ok(None) => Ok(None),
        Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -libc::ENODEV => Ok(None),
        Err(e) => Err(NetworkError::Netlink {
            op: "get_link_by_name",
            link: name.into(),
            reason: e.to_string(),
        }),
    }
}

/// Ensure an address is assigned to a link. Idempotent — if the address is
/// already there, this is a no-op.
async fn ensure_addr(
    handle: &Handle,
    index: u32,
    addr: IpAddr,
    prefix_len: u8,
    link_name: &str,
) -> crate::Result<()> {
    use netlink_packet_route::address::AddressAttribute;

    // Walk existing addresses on the link; bail early if ours is present.
    let mut addrs = handle
        .address()
        .get()
        .set_link_index_filter(index)
        .execute();
    while let Some(msg) = addrs.try_next().await.map_err(|e| NetworkError::Netlink {
        op: "list_addrs",
        link: link_name.into(),
        reason: e.to_string(),
    })? {
        for nla in &msg.attributes {
            if let AddressAttribute::Address(existing) = nla {
                if *existing == addr && msg.header.prefix_len == prefix_len {
                    return Ok(());
                }
            }
        }
    }

    handle
        .address()
        .add(index, addr, prefix_len)
        .execute()
        .await
        .map_err(|e| NetworkError::Netlink {
            op: "add_addr",
            link: link_name.into(),
            reason: e.to_string(),
        })?;
    Ok(())
}
