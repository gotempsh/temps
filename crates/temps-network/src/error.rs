//! Error type for `temps-network`.
//!
//! Following the project-wide error-handling rules: every variant carries
//! identifying context (interface name, peer node id, CIDR, etc.) so a single
//! line in a log file is enough to reproduce the failure.

use ipnet::Ipv4Net;
use std::net::IpAddr;
use thiserror::Error;
use uuid::Uuid;

/// Errors returned by the `temps-network` crate.
///
/// Variants are split by *what* failed and *where*. Callers (e.g. a future
/// `temps-agent` integration) match on these to decide whether to retry, log
/// and continue, or propagate to the control plane.
#[derive(Debug, Error)]
pub enum NetworkError {
    // ----- platform / preconditions -----
    /// Kernel data-plane operations were requested on a non-Linux host.
    #[error(
        "network data-plane operations are only supported on Linux (current target: {target})"
    )]
    UnsupportedPlatform { target: &'static str },

    /// `/proc/sys/net/ipv4/ip_forward` could not be enabled.
    #[error("failed to enable ipv4 forwarding: {reason}")]
    IpForwardFailed { reason: String },

    /// Required kernel module is missing (e.g. `vxlan` not loaded and
    /// auto-load is disabled).
    #[error("required kernel module '{module}' is unavailable: {reason}")]
    MissingKernelModule {
        module: &'static str,
        reason: String,
    },

    // ----- link / bridge -----
    /// `rtnetlink` failed while creating, modifying, or deleting a link.
    #[error("netlink operation '{op}' on link '{link}' failed: {reason}")]
    Netlink {
        op: &'static str,
        link: String,
        reason: String,
    },

    /// A bridge or device with the requested name already exists but with
    /// configuration that conflicts with what we want.
    #[error(
        "interface '{name}' exists but its configuration conflicts with the desired state: {reason}"
    )]
    InterfaceConflict { name: String, reason: String },

    // ----- transport -----
    /// VXLAN device creation or FDB management failed.
    #[error("vxlan transport error on '{device}': {reason}")]
    Vxlan { device: String, reason: String },

    // ----- routes -----
    /// Adding or removing a route failed.
    #[error("route operation '{op}' for {cidr} via {via} failed: {reason}")]
    Route {
        op: &'static str,
        cidr: Ipv4Net,
        via: String,
        reason: String,
    },

    // ----- firewall -----
    /// nftables rule installation failed.
    #[error("nftables operation '{op}' on table '{table}' failed: {reason}")]
    Nftables {
        op: &'static str,
        table: String,
        reason: String,
    },

    // ----- docker -----
    /// Bollard returned an error while creating or inspecting the Docker
    /// network.
    #[error("docker network operation '{op}' on network '{network}' failed: {reason}")]
    Docker {
        op: &'static str,
        network: String,
        reason: String,
    },

    /// The CIDR we were asked to use for our Docker network is already in use
    /// by a different Docker network on this host.
    #[error(
        "cidr {cidr} is already used by docker network '{existing_network}'; \
         refusing to create '{desired_network}' to avoid corruption"
    )]
    DockerCidrCollision {
        cidr: Ipv4Net,
        existing_network: String,
        desired_network: String,
    },

    // ----- config / validation -----
    /// The provided config was internally inconsistent (e.g. peer CIDR
    /// overlaps the node's own CIDR).
    #[error("invalid network configuration: {reason}")]
    InvalidConfig { reason: String },

    /// A peer entry referenced a node id that we do not know about, or had
    /// an invalid underlay address.
    #[error("invalid peer entry for node {node_id}: {reason}")]
    InvalidPeer { node_id: Uuid, reason: String },

    /// Underlay address resolution failed.
    #[error("could not resolve underlay address {addr} for node {node_id}: {reason}")]
    UnderlayUnreachable {
        node_id: Uuid,
        addr: IpAddr,
        reason: String,
    },

    // ----- generic IO escape hatch -----
    /// Filesystem operation failed (e.g. writing to `/proc/sys/...`).
    #[error("io error during '{op}' at '{path}': {reason}")]
    Io {
        op: &'static str,
        path: String,
        reason: String,
    },
}

#[cfg(target_os = "linux")]
impl From<rtnetlink::Error> for NetworkError {
    fn from(e: rtnetlink::Error) -> Self {
        // Default mapping; call sites that have more context should wrap with
        // a dedicated variant via `map_err` instead of relying on this `From`.
        NetworkError::Netlink {
            op: "rtnetlink",
            link: String::from("(unknown)"),
            reason: e.to_string(),
        }
    }
}

// We deliberately do *not* implement `From<bollard::errors::Error>`: every
// Docker call site has a clear notion of which network and which operation
// it is performing, so we want each call site to map errors with that
// context using `map_err` to produce `NetworkError::Docker { op, network, reason }`.
