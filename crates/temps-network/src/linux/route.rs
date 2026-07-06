//! Route table management.
//!
//! For VXLAN we add routes "via dev <vxlan>"; the kernel forwards into the
//! bridge it's enslaved to. For Native transport we use a regular gateway
//! route ("via <peer underlay>").

use crate::error::NetworkError;
use crate::linux::bridge::link_index_by_name;
use ipnet::Ipv4Net;
use rtnetlink::{Handle, RouteMessageBuilder};
use std::net::{IpAddr, Ipv4Addr};
use tracing::{debug, warn};

/// Add a route for `cidr` that egresses via `dev`. Idempotent — if the route
/// already exists, this is a no-op.
///
/// `pref_src` (when set) becomes the route's preferred source — the IP
/// the kernel will use as the ARP source / source IP for traffic that
/// hits this route. Required for VXLAN routes: without it the kernel
/// falls back to the underlay IP (eth0), which is in the wrong subnet
/// and causes peer workers to drop the inner ARP / SYN.
pub async fn add_via_dev(
    handle: &Handle,
    cidr: Ipv4Net,
    dev: &str,
    pref_src: Option<Ipv4Addr>,
) -> crate::Result<()> {
    let dev_idx = link_index_by_name(handle, dev)
        .await?
        .ok_or(NetworkError::Route {
            op: "add_via_dev",
            cidr,
            via: dev.into(),
            reason: format!("device '{}' not found", dev),
        })?;

    let mut builder = RouteMessageBuilder::<Ipv4Addr>::new()
        .destination_prefix(cidr.network(), cidr.prefix_len())
        .output_interface(dev_idx);
    if let Some(src) = pref_src {
        builder = builder.pref_source(src);
    }
    let res = handle.route().add(builder.build()).execute().await;

    handle_route_result(res, "add_via_dev", cidr, dev)
}

/// Add a route for `cidr` via gateway `gw`. Idempotent.
pub async fn add_via_gateway(handle: &Handle, cidr: Ipv4Net, gw: IpAddr) -> crate::Result<()> {
    let gw_v4 = match gw {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(_) => {
            return Err(NetworkError::Route {
                op: "add_via_gateway",
                cidr,
                via: gw.to_string(),
                reason: "ipv6 gateway not supported".into(),
            })
        }
    };

    let res = handle
        .route()
        .add(
            RouteMessageBuilder::<Ipv4Addr>::new()
                .destination_prefix(cidr.network(), cidr.prefix_len())
                .gateway(gw_v4)
                .build(),
        )
        .execute()
        .await;

    handle_route_result(res, "add_via_gateway", cidr, &gw.to_string())
}

/// Remove a route for `cidr`, regardless of how it was installed. Idempotent.
pub async fn remove(handle: &Handle, cidr: Ipv4Net) -> crate::Result<()> {
    use futures::TryStreamExt;
    use netlink_packet_route::route::{RouteAddress, RouteAttribute};

    let mut routes = handle
        .route()
        .get(RouteMessageBuilder::<Ipv4Addr>::new().build())
        .execute();
    while let Some(msg) = routes.try_next().await.map_err(|e| NetworkError::Route {
        op: "list_routes",
        cidr,
        via: "(any)".into(),
        reason: e.to_string(),
    })? {
        let prefix = msg.header.destination_prefix_length;
        let mut dst: Option<Ipv4Addr> = None;
        for attr in &msg.attributes {
            if let RouteAttribute::Destination(RouteAddress::Inet(v4)) = attr {
                dst = Some(*v4);
            }
        }
        if let Some(d) = dst {
            if d == cidr.network() && prefix == cidr.prefix_len() {
                if let Err(e) = handle.route().del(msg.clone()).execute().await {
                    return Err(NetworkError::Route {
                        op: "remove",
                        cidr,
                        via: "(any)".into(),
                        reason: e.to_string(),
                    });
                }
                return Ok(());
            }
        }
    }
    debug!(%cidr, "route already absent");
    Ok(())
}

fn handle_route_result(
    res: Result<(), rtnetlink::Error>,
    op: &'static str,
    cidr: Ipv4Net,
    via: &str,
) -> crate::Result<()> {
    match res {
        Ok(()) => Ok(()),
        Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -libc::EEXIST => {
            warn!(%cidr, %via, "route already exists");
            Ok(())
        }
        Err(e) => Err(NetworkError::Route {
            op,
            cidr,
            via: via.into(),
            reason: e.to_string(),
        }),
    }
}
