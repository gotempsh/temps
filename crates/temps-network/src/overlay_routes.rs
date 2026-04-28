//! Inject per-peer routes into a container's network namespace so that
//! traffic destined for *another* worker's overlay `/24` leaves through
//! the local overlay interface (eth1) instead of falling through the
//! container's default route on the primary network.
//!
//! Why this is needed
//! ------------------
//! Docker's IPAM gives each container a single `/24` subnet on the
//! overlay (`temps0`) — the worker's own `compute_cidr`. The container's
//! kernel learns:
//!   - `default via <eth0_gateway> dev eth0`     (primary network)
//!   - `<own /24> dev eth1 proto kernel`         (overlay, this worker)
//!   - `<own /24> dev eth0 proto kernel`         (other side, primary)
//!
//! When the container dials a peer at e.g. `172.20.5.2`, the kernel
//! has no route for `172.20.5.0/24`, falls through to default, sources
//! the connection from the primary IP, and packets either get dropped
//! at the worker's iptables FORWARD chain or take the wrong path
//! entirely.
//!
//! The fix is one route per peer (`<peer.compute_cidr> via <local
//! gateway> dev eth1`). The kernel then sources the connection from
//! the overlay IP and hands the packet to `br-temps0`, which the
//! VXLAN data plane carries to the right worker.
//!
//! We inject the routes *after* Docker has connected the container to
//! the overlay — the `eth1` interface and its IP only exist after
//! `docker network connect` has run.

use crate::config::Peer;
use crate::error::NetworkError;
use ipnet::Ipv4Net;
use std::process::Stdio;
use tracing::{debug, warn};

/// Add `peer.compute_cidr → <gateway> dev <iface>` routes to the
/// container's network namespace, idempotently. Already-present routes
/// are left in place. Failures for individual peers are logged and
/// skipped — the function never errors out before walking the whole
/// list, because partial routing is strictly better than none.
///
/// `pid` is the container's main process PID inside the agent host's
/// PID namespace (read from `docker inspect`). The agent runs with
/// CAP_NET_ADMIN, which is what `nsenter -n` + `ip route add` needs.
///
/// The function discovers the overlay-side interface by name (eth1 by
/// convention — Docker creates one veth per attached network in
/// attach order, and the overlay is always the second attach). If the
/// expected interface isn't there yet, we wait a short time and retry,
/// since `connect_network` returns before veth creation has fully
/// propagated.
pub async fn install_peer_routes_in_container(
    container_pid: i32,
    _iface_hint: &str,
    local_gateway: &str,
    peers: &[Peer],
) -> Result<(), NetworkError> {
    if peers.is_empty() {
        return Ok(());
    }

    // Discover the overlay interface inside the container's netns by
    // matching its IP against the local gateway's /24. We can't trust
    // a fixed name like "eth1" because Docker's interface ordering
    // depends on attach order, sort order in `NetworkSettings.Networks`,
    // and image conventions — none of those are stable. Matching by IP
    // is unambiguous: the only interface in the gateway's subnet IS
    // the overlay one.
    let mut iface_name: Option<String> = None;
    let mut tries = 0;
    while tries < 20 {
        if let Some(name) = find_iface_for_gateway(container_pid, local_gateway).await {
            iface_name = Some(name);
            break;
        }
        tries += 1;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let Some(iface) = iface_name else {
        warn!(
            container_pid,
            local_gateway,
            "Could not find the container's overlay interface; \
             skipping peer-route injection (cross-worker traffic will fail)"
        );
        return Ok(());
    };

    for peer in peers {
        if let Err(e) = add_route(container_pid, &peer.compute_cidr, local_gateway, &iface).await {
            warn!(
                container_pid,
                peer_cidr = %peer.compute_cidr,
                error = %e,
                "Failed to add peer route inside container netns"
            );
            continue;
        }
        debug!(
            container_pid,
            peer_cidr = %peer.compute_cidr,
            via = %local_gateway,
            %iface,
            "Installed peer route in container netns"
        );
    }

    Ok(())
}

/// Find the interface inside the container's netns whose IPv4 address
/// shares a `/24` with `gateway`. Returns the interface name (e.g.
/// `eth0`, `eth1`) or `None` if nothing matches yet.
async fn find_iface_for_gateway(container_pid: i32, gateway: &str) -> Option<String> {
    let gw_v4: std::net::Ipv4Addr = gateway.parse().ok()?;
    let gw_octets = gw_v4.octets();

    let output = tokio::process::Command::new("nsenter")
        .arg("-t")
        .arg(container_pid.to_string())
        .arg("-n")
        .arg("ip")
        .arg("-o")
        .arg("-4")
        .arg("addr")
        .arg("show")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Each line looks like:
    //   3: eth0    inet 172.20.0.2/24 brd 172.20.0.255 scope global eth0\       valid_lft forever preferred_lft forever
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        // index, ifname, family, addr/prefix
        let _idx = parts.next();
        let ifname = match parts.next() {
            Some(n) => n.trim_end_matches(':'),
            None => continue,
        };
        if ifname == "lo" {
            continue;
        }
        let _family = parts.next();
        let addr = match parts.next() {
            Some(a) => a,
            None => continue,
        };
        let ip_str = addr.split('/').next().unwrap_or("");
        let Ok(ip): Result<std::net::Ipv4Addr, _> = ip_str.parse() else {
            continue;
        };
        let octets = ip.octets();
        if octets[0] == gw_octets[0] && octets[1] == gw_octets[1] && octets[2] == gw_octets[2] {
            return Some(ifname.to_string());
        }
    }
    None
}

/// Add `<cidr> via <gateway> dev <iface>` if not already present.
/// Idempotent: an existing route returns successfully (we treat
/// "RTNETLINK answers: File exists" as a no-op).
async fn add_route(
    container_pid: i32,
    cidr: &Ipv4Net,
    gateway: &str,
    iface: &str,
) -> Result<(), NetworkError> {
    let output = tokio::process::Command::new("nsenter")
        .arg("-t")
        .arg(container_pid.to_string())
        .arg("-n")
        .arg("ip")
        .arg("route")
        .arg("replace")
        .arg(cidr.to_string())
        .arg("via")
        .arg(gateway)
        .arg("dev")
        .arg(iface)
        .output()
        .await
        .map_err(|e| NetworkError::Io {
            op: "nsenter ip route replace",
            path: format!("pid={} cidr={}", container_pid, cidr),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(NetworkError::Io {
            op: "ip route replace",
            path: format!("pid={} cidr={} dev={}", container_pid, cidr, iface),
            reason: stderr.trim().to_string(),
        });
    }
    Ok(())
}
