// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Classifying the address a cluster node registered with.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Whether a node's registered address (`ip` or `ip:port`) is private: RFC
/// 1918 or IPv6 unique-local. Only such an address may carry published
/// workload ports; a node that joined with anything else (a public IP, a
/// CGNAT/Tailscale 100.64/10 address) reaches its workloads over the
/// WireGuard mesh instead.
pub fn is_private_node_address(address: &str) -> bool {
    let address = address.trim();
    let ip = address
        .parse::<IpAddr>()
        .or_else(|_| address.parse::<SocketAddr>().map(|socket| socket.ip()));
    match ip {
        Ok(IpAddr::V4(v4)) => v4.is_private(),
        Ok(IpAddr::V6(v6)) => v6.is_unique_local(),
        Err(_) => false,
    }
}

/// Key in a node's heartbeat `capacity` under which its agent reports the
/// `ip:port` it serves its API on over the WireGuard mesh, once that listener
/// is bound. The control plane only dials the mesh for an agent that reports
/// it: an older agent (or one whose mesh is down) is still reached at its
/// registered address.
///
/// Three states on the wire (see [`carry_agent_mesh_listener`]): a string
/// (served there), `null` (the agent knows it is not served on the mesh) and
/// absent (the agent does not know yet, e.g. just after a restart, or is too
/// old to report it).
pub const AGENT_MESH_LISTENER_KEY: &str = "agent_mesh_listener";

/// Resolve the mesh listener a heartbeat leaves on the node, editing the
/// heartbeat's `capacity` in place (`previous` is the stored capacity).
///
/// A heartbeat replaces the node's capacity wholesale, but an agent sends its
/// first heartbeat right after it starts, before its mesh listener is bound.
/// Dropping the listener then would send every call in that window to the
/// registered address, which a control plane that reaches the node only
/// through the mesh can never reach. So:
///
/// - a reported address is taken as-is;
/// - an explicit `null` (or anything that is not a string) clears it: the
///   agent knows it is not served on the mesh (the mesh is gone, or the
///   listener could not bind);
/// - an omitted key keeps the stored listener only while it is on the node's
///   current mesh address (`mesh_wg_address`). A new mesh address, or the
///   mesh disabled, drops it. An agent that never reports the key never has
///   one stored, so it keeps being dialed at its registered address.
pub fn carry_agent_mesh_listener(
    capacity: &mut serde_json::Value,
    previous: &serde_json::Value,
    mesh_wg_address: Option<&str>,
) {
    let Some(fields) = capacity.as_object_mut() else {
        return;
    };
    match fields.get(AGENT_MESH_LISTENER_KEY) {
        Some(serde_json::Value::String(_)) => {}
        Some(_) => {
            fields.remove(AGENT_MESH_LISTENER_KEY);
        }
        None => {
            let mesh = mesh_wg_address.and_then(|mesh| mesh.trim().parse::<Ipv4Addr>().ok());
            let kept = previous
                .get(AGENT_MESH_LISTENER_KEY)
                .and_then(serde_json::Value::as_str)
                .filter(|listener| {
                    listener
                        .trim()
                        .parse::<SocketAddr>()
                        .is_ok_and(|listener| mesh.is_some_and(|mesh| listener.ip() == mesh))
                });
            if let Some(listener) = kept {
                fields.insert(
                    AGENT_MESH_LISTENER_KEY.to_string(),
                    serde_json::Value::String(listener.to_string()),
                );
            }
        }
    }
}

/// Whether a node agent address uses TLS (`https://`, scheme compared
/// case-insensitively as RFC 3986 requires).
pub fn is_https_agent_address(address: &str) -> bool {
    address
        .trim_start()
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

/// Where the control plane opens a connection to a node's agent API, and the
/// name it verifies the agent's certificate for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAgentEndpoint {
    /// Base URL requests go to (`https://<ip>:<port>`, no trailing slash
    /// added or removed).
    pub dial_url: String,
    /// `Some(host)` when `dial_url` is not the registered address: the agent's
    /// certificate must still be valid for `host`, the host of the registered
    /// address, because the leaf the cluster CA signed names the node's
    /// registered addresses and not its mesh address. `None` means the
    /// registered address is dialed and verified as-is.
    pub server_name: Option<String>,
}

impl NodeAgentEndpoint {
    /// Dial and verify the registered address unchanged.
    pub fn direct(address: &str) -> Self {
        Self {
            dial_url: address.to_string(),
            server_name: None,
        }
    }

    /// Whether the connection goes over the WireGuard mesh rather than to the
    /// registered address.
    pub fn via_mesh(&self) -> bool {
        self.server_name.is_some()
    }
}

/// Decide how the control plane reaches a node's agent API.
///
/// A node whose agent reports serving on its current WireGuard mesh address
/// is reached there, whatever address it registered with. That report is the
/// only thing the control plane knows the agent serves: the registered
/// address names the node's private address, but the agent's main listener
/// is wherever `--agent-address` put it, by default loopback (a paired or
/// outbound-joined worker never serves on its private address at all). So
/// the control plane never needs a route to the node's address, a control
/// plane behind NAT that can only reach its workers through the mesh still
/// can, and the call is encrypted on a shared private network too.
/// Everything else is dialed at its registered `address`:
///
/// - plaintext (`http://`) agents: the mesh path is only ever used with mutual
///   TLS, so a plaintext agent is never exposed on another interface;
/// - nodes registered under their mesh address (an outbound join, ADR 055):
///   the registered address already is the mesh path;
/// - nodes whose agent has not reported a mesh listener on its current mesh
///   address (`agent_mesh_listener`, see [`AGENT_MESH_LISTENER_KEY`]): an
///   agent too old to serve on the mesh, or whose mesh is not up yet.
///
/// The mesh dial keeps the agent's port as the agent reported it, and the
/// certificate is verified for the registered host (see
/// [`NodeAgentEndpoint::server_name`]).
pub fn node_agent_endpoint(
    address: &str,
    mesh_wg_address: Option<&str>,
    agent_mesh_listener: Option<&str>,
) -> NodeAgentEndpoint {
    let direct = || NodeAgentEndpoint::direct(address);
    if !is_https_agent_address(address) {
        return direct();
    }
    let Some(mesh) = mesh_wg_address.and_then(|mesh| mesh.trim().parse::<Ipv4Addr>().ok()) else {
        return direct();
    };
    let Some(listener) =
        agent_mesh_listener.and_then(|listener| listener.trim().parse::<SocketAddr>().ok())
    else {
        return direct();
    };
    if listener.ip() != IpAddr::V4(mesh) || listener.port() == 0 {
        return direct();
    }
    let Some(host) = registered_host(address) else {
        return direct();
    };
    if host.parse::<IpAddr>().is_ok_and(|ip| ip == listener.ip()) {
        return direct();
    }
    NodeAgentEndpoint {
        dial_url: format!("https://{listener}"),
        server_name: Some(host),
    }
}

/// The host of an `https://host[:port][/path]` agent address, without IPv6
/// brackets: the name the agent's certificate is issued for.
fn registered_host(address: &str) -> Option<String> {
    let url = url::Url::parse(address.trim()).ok()?;
    match url.host()? {
        url::Host::Domain(domain) => Some(domain.to_string()),
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rfc1918_and_unique_local_addresses_are_private() {
        for private in [
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.9",
            "fd00::1",
            "10.0.0.5:3100",
            "[fd00::1]:3100",
        ] {
            assert!(is_private_node_address(private), "{private}");
        }
        for public in [
            "203.0.113.10",
            "8.8.8.8",
            "100.101.102.103",
            "2001:db8::1",
            "not-an-ip",
            "",
        ] {
            assert!(!is_private_node_address(public), "{public}");
        }
    }

    const PUBLIC: &str = "https://203.0.113.10:3100";

    #[test]
    fn a_public_node_with_a_mesh_listener_is_dialed_on_the_mesh() {
        let endpoint = node_agent_endpoint(PUBLIC, Some("10.201.0.5"), Some("10.201.0.5:3100"));
        assert_eq!(
            endpoint,
            NodeAgentEndpoint {
                dial_url: "https://10.201.0.5:3100".into(),
                server_name: Some("203.0.113.10".into()),
            }
        );
        assert!(endpoint.via_mesh());
    }

    #[test]
    fn the_mesh_dial_uses_the_port_the_agent_listens_on() {
        let endpoint = node_agent_endpoint(
            "https://worker.example.com:443",
            Some("10.201.0.5"),
            Some("10.201.0.5:3100"),
        );
        assert_eq!(endpoint.dial_url, "https://10.201.0.5:3100");
        assert_eq!(endpoint.server_name.as_deref(), Some("worker.example.com"));
    }

    /// ADR 055: a worker that joined outbound only registered its own mesh
    /// address as its address and private address. That is private, so it
    /// is dialed there directly, which is over the tunnel it opened, whether
    /// or not its agent reported the mesh listener yet.
    #[test]
    fn a_node_registered_under_its_mesh_address_is_dialed_there() {
        for listener in [Some("10.201.0.5:3100"), None] {
            let endpoint =
                node_agent_endpoint("https://10.201.0.5:3100", Some("10.201.0.5"), listener);
            assert_eq!(
                endpoint,
                NodeAgentEndpoint::direct("https://10.201.0.5:3100"),
                "{listener:?}"
            );
            assert!(!endpoint.via_mesh());
        }
    }

    /// A paired worker registers its private address, but its agent listens
    /// on loopback and the mesh only (`--agent-address` defaults to
    /// loopback). A control plane on the same private network must still
    /// dial the mesh listener the agent reported, never the registered
    /// address it does not serve.
    #[test]
    fn a_private_network_node_with_a_mesh_listener_is_dialed_on_the_mesh() {
        let endpoint = node_agent_endpoint(
            "https://10.77.0.11:3100",
            Some("10.201.0.2"),
            Some("10.201.0.2:3100"),
        );
        assert_eq!(
            endpoint,
            NodeAgentEndpoint {
                dial_url: "https://10.201.0.2:3100".into(),
                server_name: Some("10.77.0.11".into()),
            }
        );
        assert!(endpoint.via_mesh());
    }

    /// address, mesh address, reported listener, why.
    type Case<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a str);

    #[test]
    fn everything_else_is_dialed_at_the_registered_address() {
        let cases: &[Case] = &[
            (
                "https://10.0.0.5:3100",
                Some("10.201.0.5"),
                None,
                "private network, agent reports no mesh listener",
            ),
            (PUBLIC, None, Some("10.201.0.5:3100"), "no mesh address"),
            (
                "http://203.0.113.10:3100",
                Some("10.201.0.5"),
                Some("10.201.0.5:3100"),
                "plaintext agent",
            ),
            (
                PUBLIC,
                Some("10.201.0.5"),
                None,
                "agent reports no mesh listener",
            ),
            (
                PUBLIC,
                Some("10.201.0.5"),
                Some("10.201.0.9:3100"),
                "listener on another mesh address",
            ),
            (
                PUBLIC,
                Some("10.201.0.5"),
                Some("10.201.0.5:0"),
                "listener without a port",
            ),
            (
                PUBLIC,
                Some("not-an-ip"),
                Some("10.201.0.5:3100"),
                "bad mesh address",
            ),
            (
                PUBLIC,
                Some("10.201.0.5"),
                Some("garbage"),
                "unparsable listener",
            ),
        ];
        for (address, mesh, listener, why) in cases {
            assert_eq!(
                node_agent_endpoint(address, *mesh, *listener),
                NodeAgentEndpoint::direct(address),
                "{why}"
            );
        }
    }

    #[test]
    fn the_registered_host_of_an_ipv6_address_has_no_brackets() {
        let endpoint = node_agent_endpoint(
            "HTTPS://[2001:db8::10]:3100",
            Some("10.201.0.5"),
            Some("10.201.0.5:3100"),
        );
        assert_eq!(endpoint.server_name.as_deref(), Some("2001:db8::10"));
    }

    #[test]
    fn https_detection_ignores_scheme_case_and_leading_space() {
        assert!(is_https_agent_address("https://10.0.0.1:3100"));
        assert!(is_https_agent_address("HTTPS://10.0.0.1:3100"));
        assert!(is_https_agent_address("  https://node-1:3100"));
        assert!(!is_https_agent_address("http://10.0.0.1:3100"));
        assert!(!is_https_agent_address("https:/10.0.0.1"));
        assert!(!is_https_agent_address(""));
    }

    #[test]
    fn a_heartbeat_without_the_listener_keeps_it_while_the_mesh_address_holds() {
        use serde_json::json;
        let stored = json!({"cpu": 1, AGENT_MESH_LISTENER_KEY: "10.201.0.2:3100"});
        let carried = |mut beat: serde_json::Value, mesh: Option<&str>| {
            carry_agent_mesh_listener(&mut beat, &stored, mesh);
            beat.get(AGENT_MESH_LISTENER_KEY).cloned()
        };

        // Just restarted, listener not bound yet: keep calling over the mesh.
        assert_eq!(
            carried(json!({"cpu": 2}), Some("10.201.0.2")),
            Some(json!("10.201.0.2:3100"))
        );
        // A reported listener wins.
        assert_eq!(
            carried(
                json!({AGENT_MESH_LISTENER_KEY: "10.201.0.2:3443"}),
                Some("10.201.0.2")
            ),
            Some(json!("10.201.0.2:3443"))
        );
        // The agent says it is not served on the mesh.
        assert_eq!(
            carried(json!({AGENT_MESH_LISTENER_KEY: null}), Some("10.201.0.2")),
            None
        );
        assert_eq!(
            carried(json!({AGENT_MESH_LISTENER_KEY: 7}), Some("10.201.0.2")),
            None
        );
        // New mesh address, or no mesh: the old listener is gone.
        assert_eq!(carried(json!({"cpu": 2}), Some("10.201.0.9")), None);
        assert_eq!(carried(json!({"cpu": 2}), None), None);
        assert_eq!(carried(json!({"cpu": 2}), Some("garbage")), None);
    }

    #[test]
    fn an_agent_that_never_reported_a_listener_is_never_given_one() {
        use serde_json::json;
        let mut beat = json!({"cpu": 2});
        carry_agent_mesh_listener(&mut beat, &json!({"cpu": 1}), Some("10.201.0.2"));
        assert_eq!(beat, json!({"cpu": 2}));

        let mut beat = json!({"cpu": 2});
        carry_agent_mesh_listener(
            &mut beat,
            &json!({AGENT_MESH_LISTENER_KEY: "not an address"}),
            Some("10.201.0.2"),
        );
        assert_eq!(beat, json!({"cpu": 2}));

        // A capacity that is not an object is left alone.
        let mut beat = json!(null);
        carry_agent_mesh_listener(&mut beat, &json!({}), Some("10.201.0.2"));
        assert_eq!(beat, json!(null));
    }
}
