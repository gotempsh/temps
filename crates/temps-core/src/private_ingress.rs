// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's private ingress (ADR-115 D3).
//!
//! When the operator consents, the control plane serves private addresses
//! on its proxy's own listener: a connection whose **local** address is the
//! consented private address, and whose **TCP peer** is inside that
//! address's range, is matched against node `0`'s private routes only.
//! Anything else on that address gets the private listener's refusals.
//!
//! The proxy reads [`ControlPlanePrivateIngress`] per connection (one
//! `ArcSwap` load, no lock); the control plane's private-ingress loop sets
//! the consented address and reads which addresses the proxy listens on.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use arc_swap::{ArcSwap, ArcSwapOption};
use tokio::sync::Notify;

use ipnet::IpNet;

use crate::private_network::PrivateNetworkOption;
use crate::worker_ingress::{check_private_listen, PRIVATE_HTTPS_PORT, PRIVATE_HTTP_PORT};

/// The address the control plane serves private addresses on, once
/// consented and found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateIngressServing {
    /// The consented private address (`10.8.0.1`, `100.101.102.103`).
    pub address: IpAddr,
    /// The ranges a visitor's TCP peer must be in (ADR-115 D8): the
    /// interface's subnet, Tailscale's ranges, or what the operator named.
    pub admit: Vec<IpNet>,
}

impl PrivateIngressServing {
    /// The serving state for `address` admitting `admit`, or why it cannot
    /// be one.
    pub fn new(address: IpAddr, admit: Vec<IpNet>) -> Result<Self, String> {
        let address = canonical(address);
        check_private_listen(address)?;
        if admit.is_empty() {
            return Err(format!(
                "{address} would admit nobody: no range is admitted"
            ));
        }
        Ok(Self { address, admit })
    }

    fn admits(&self, peer: IpAddr) -> bool {
        let peer = canonical(peer);
        self.admit.iter().any(|range| range.contains(&peer))
    }
}

/// How a connection arrived, for the private path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateArrival {
    /// Not on the private address: the public path, as always.
    Public,
    /// On the private address from inside its ranges: node `0`'s private
    /// routes only.
    Admitted,
    /// On the private address from outside its range: refused.
    OutsideRange,
}

/// What the proxy and the private-ingress loop share.
#[derive(Debug, Default)]
pub struct ControlPlanePrivateIngress {
    serving: ArcSwapOption<PrivateIngressServing>,
    /// This machine's private networks, as the loop last found them, for
    /// the console to offer (ADR-115 D8).
    networks: ArcSwap<Vec<PrivateNetworkOption>>,
    /// Every address the proxy's HTTP and HTTPS listeners bound, set once
    /// at startup.
    proxy_listeners: OnceLock<Vec<SocketAddr>>,
    /// Woken when the consent changes, so the loop applies it at once, and
    /// when the proxy records its listeners.
    changed: Notify,
}

impl ControlPlanePrivateIngress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record what the proxy's listeners bound. Only the first call counts:
    /// Pingora binds listeners once, at startup. It wakes the loop, which
    /// waits for this before its first pass.
    pub fn set_proxy_listeners(&self, listeners: Vec<SocketAddr>) {
        if self.proxy_listeners.set(listeners).is_ok() {
            self.changed.notify_one();
        }
    }

    /// What the proxy's listeners bound, `None` when no proxy runs in this
    /// process.
    pub fn proxy_listeners(&self) -> Option<&[SocketAddr]> {
        self.proxy_listeners.get().map(Vec::as_slice)
    }

    /// Serve private addresses on `serving`, or stop with `None`.
    pub fn set_serving(&self, serving: Option<PrivateIngressServing>) {
        let unchanged = self.serving.load().as_deref() == serving.as_ref();
        if !unchanged {
            self.serving.store(serving.map(Arc::new));
        }
    }

    /// The address private addresses are served on now.
    pub fn serving(&self) -> Option<PrivateIngressServing> {
        self.serving.load().as_deref().cloned()
    }

    /// Record this machine's private networks.
    pub fn set_networks(&self, networks: Vec<PrivateNetworkOption>) {
        if **self.networks.load() != networks {
            self.networks.store(Arc::new(networks));
        }
    }

    /// This machine's private networks, as last found.
    pub fn networks(&self) -> Vec<PrivateNetworkOption> {
        self.networks.load().as_ref().clone()
    }

    /// Wake the loop: the consent changed.
    pub fn notify_changed(&self) {
        self.changed.notify_one();
    }

    /// Wait until [`Self::notify_changed`] is called.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// How a connection accepted on `local` from `peer` arrived. The peer is
    /// the TCP peer, never a forwarded header.
    pub fn arrival(&self, local: Option<SocketAddr>, peer: Option<IpAddr>) -> PrivateArrival {
        let guard = self.serving.load();
        let (Some(serving), Some(local)) = (guard.as_deref(), local) else {
            return PrivateArrival::Public;
        };
        if canonical(local.ip()) != serving.address {
            return PrivateArrival::Public;
        }
        match peer {
            Some(peer) if serving.admits(peer) => PrivateArrival::Admitted,
            _ => PrivateArrival::OutsideRange,
        }
    }

    /// Why the proxy's listeners do not take connections to `address` on
    /// ports 443 and 80, or `None` when they do. Pingora binds only at
    /// startup, so the fix is a restart (ADR-115 D3).
    pub fn not_covered(&self, address: IpAddr) -> Option<String> {
        let Some(listeners) = self.proxy_listeners() else {
            return Some(
                "No proxy runs in this process, so it cannot serve private addresses. Run \
                 `temps serve` with the proxy (the default role)."
                    .to_string(),
            );
        };
        not_covered_by(listeners, address)
    }
}

/// [`ControlPlanePrivateIngress::not_covered`] on an explicit list.
pub fn not_covered_by(listeners: &[SocketAddr], address: IpAddr) -> Option<String> {
    let address = canonical(address);
    let covers = |port: u16| {
        listeners
            .iter()
            .any(|l| l.port() == port && listener_takes(l.ip(), address))
    };
    if covers(PRIVATE_HTTPS_PORT) {
        // Port 80 only redirects; its absence is not a reason to refuse.
        return None;
    }
    let shown: Vec<String> = listeners.iter().map(ToString::to_string).collect();
    Some(format!(
        "The proxy listens on {} only, so it does not take connections to {address}:{PRIVATE_HTTPS_PORT}. \
         Restart `temps serve` with `--tls-address=0.0.0.0:{PRIVATE_HTTPS_PORT}` (and \
         `--address=0.0.0.0:{PRIVATE_HTTP_PORT}`), or with `--tls-address={address}:{PRIVATE_HTTPS_PORT}`, \
         to serve private addresses.",
        if shown.is_empty() {
            "no address".to_string()
        } else {
            shown.join(", ")
        }
    ))
}

/// Whether a listener bound on `bound` accepts connections to `address`.
/// `[::]` takes IPv4 too, as Linux binds it dual-stack by default.
fn listener_takes(bound: IpAddr, address: IpAddr) -> bool {
    match bound {
        IpAddr::V4(v4) if v4.is_unspecified() => address.is_ipv4(),
        IpAddr::V6(v6) if v6.is_unspecified() => true,
        other => canonical(other) == address,
    }
}

/// An IPv4-mapped IPv6 address as IPv4; anything else unchanged.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn at(text: &str) -> Option<SocketAddr> {
        Some(text.parse().unwrap())
    }

    fn tailscale() -> Vec<IpNet> {
        vec!["100.64.0.0/10".parse().unwrap()]
    }

    #[test]
    fn nothing_is_private_until_an_address_is_served() {
        let ingress = ControlPlanePrivateIngress::new();
        assert_eq!(
            ingress.arrival(at("100.101.102.103:443"), Some(ip("100.64.0.9"))),
            PrivateArrival::Public
        );
        ingress.set_serving(Some(
            PrivateIngressServing::new(ip("100.101.102.103"), tailscale()).unwrap(),
        ));
        assert_eq!(
            ingress.arrival(at("100.101.102.103:443"), Some(ip("100.64.0.9"))),
            PrivateArrival::Admitted
        );
        ingress.set_serving(None);
        assert_eq!(
            ingress.arrival(at("100.101.102.103:443"), Some(ip("100.64.0.9"))),
            PrivateArrival::Public
        );
    }

    #[test]
    fn both_locks_the_local_address_and_the_peer_range() {
        let ingress = ControlPlanePrivateIngress::new();
        ingress.set_serving(Some(
            PrivateIngressServing::new(ip("100.101.102.103"), tailscale()).unwrap(),
        ));
        // Another local address: the public path.
        assert_eq!(
            ingress.arrival(at("203.0.113.7:443"), Some(ip("100.64.0.9"))),
            PrivateArrival::Public
        );
        // The private address reached from the LAN (the weak host model):
        // refused, whatever headers say.
        assert_eq!(
            ingress.arrival(at("100.101.102.103:443"), Some(ip("192.168.1.30"))),
            PrivateArrival::OutsideRange
        );
        assert_eq!(
            ingress.arrival(at("100.101.102.103:443"), None),
            PrivateArrival::OutsideRange
        );
        // A dual-stack socket reports IPv4-mapped addresses.
        assert_eq!(
            ingress.arrival(
                at("[::ffff:100.101.102.103]:443"),
                Some(ip("::ffff:100.64.0.9"))
            ),
            PrivateArrival::Admitted
        );
    }

    #[test]
    fn only_a_private_address_can_be_served() {
        for refused in ["0.0.0.0", "::", "127.0.0.1", "203.0.113.7"] {
            assert!(
                PrivateIngressServing::new(ip(refused), tailscale()).is_err(),
                "{refused}"
            );
        }
        assert!(PrivateIngressServing::new(ip("192.168.1.20"), vec![]).is_err());
    }

    #[test]
    fn a_lan_address_admits_its_subnet_not_the_whole_block() {
        let ingress = ControlPlanePrivateIngress::new();
        ingress.set_serving(Some(
            PrivateIngressServing::new(ip("192.168.1.20"), vec!["192.168.1.0/24".parse().unwrap()])
                .unwrap(),
        ));
        assert_eq!(
            ingress.arrival(at("192.168.1.20:443"), Some(ip("192.168.1.30"))),
            PrivateArrival::Admitted
        );
        // Another network in the same private block is outside.
        assert_eq!(
            ingress.arrival(at("192.168.1.20:443"), Some(ip("192.168.64.2"))),
            PrivateArrival::OutsideRange
        );
    }

    #[test]
    fn the_proxy_must_listen_where_the_private_address_is() {
        let tailscale = ip("100.101.102.103");
        let listeners = |list: &[&str]| -> Vec<SocketAddr> {
            list.iter().map(|l| l.parse().unwrap()).collect()
        };
        assert_eq!(
            not_covered_by(&listeners(&["0.0.0.0:80", "0.0.0.0:443"]), tailscale),
            None
        );
        assert_eq!(not_covered_by(&listeners(&["[::]:443"]), tailscale), None);
        assert_eq!(
            not_covered_by(&listeners(&["100.101.102.103:443"]), tailscale),
            None
        );
        let specific = not_covered_by(&listeners(&["203.0.113.7:443"]), tailscale).unwrap();
        assert!(specific.contains("203.0.113.7:443 only"), "{specific}");
        assert!(specific.contains("--tls-address=0.0.0.0:443"), "{specific}");
        let other_port = not_covered_by(&listeners(&["0.0.0.0:8443"]), tailscale).unwrap();
        assert!(other_port.contains("0.0.0.0:8443"), "{other_port}");

        let ingress = ControlPlanePrivateIngress::new();
        assert!(ingress
            .not_covered(tailscale)
            .unwrap()
            .contains("No proxy runs"));
        ingress.set_proxy_listeners(listeners(&["0.0.0.0:443"]));
        assert_eq!(ingress.not_covered(tailscale), None);
    }

    #[tokio::test]
    async fn recording_the_proxy_listeners_wakes_the_loop_once() {
        let ingress = ControlPlanePrivateIngress::new();
        ingress.set_proxy_listeners(vec!["0.0.0.0:443".parse().unwrap()]);
        tokio::time::timeout(std::time::Duration::from_secs(1), ingress.changed())
            .await
            .expect("the first record wakes the loop");
        // Pingora binds once; a later call changes nothing and wakes nobody.
        ingress.set_proxy_listeners(vec!["0.0.0.0:8443".parse().unwrap()]);
        assert_eq!(
            ingress.proxy_listeners(),
            Some(&["0.0.0.0:443".parse().unwrap()][..])
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), ingress.changed())
                .await
                .is_err()
        );
    }
}
