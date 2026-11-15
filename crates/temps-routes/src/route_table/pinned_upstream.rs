// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The checked addresses a custom route's upstream hostname is dialled at.
//!
//! A custom route may name its upstream by hostname. Checking that name when
//! the route is saved or loaded says nothing about the address a later
//! request reaches if the proxy resolves the name again when it connects:
//! the name can answer with a public address for the check and an internal
//! one afterwards.
//!
//! So the name is resolved here, off the request path, and only addresses
//! that pass [`temps_core::route_policy`] are kept. A request is sent to one
//! of those addresses, never to the name. [`spawn_refresher`] re-checks every
//! pinned name each [`REFRESH_INTERVAL`], so a legitimate address change is
//! followed within that interval and a name that starts answering with an
//! internal address stops being served.
//!
//! On the request path this costs one atomic load and one counter increment;
//! there is no lookup and no lock.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use temps_core::route_policy::{resolve_upstream, RouteUpstreamError};
use tracing::{debug, info, warn};

/// How often every pinned name is resolved and checked again.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// How many names are resolved at once during a refresh.
const REFRESH_CONCURRENCY: usize = 8;

/// What a pinned upstream may currently be dialled at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamAddresses {
    /// The name has not resolved yet (a resolver error at load time). Nothing
    /// is dialled until a refresh resolves it.
    Pending,
    /// Addresses that passed the check, in resolver order.
    Allowed(Vec<SocketAddr>),
    /// The last answer contained an address the policy forbids.
    Refused(String),
}

/// A custom route upstream named by hostname, with the addresses it may be
/// dialled at.
#[derive(Debug)]
pub struct PinnedUpstream {
    host: String,
    port: u16,
    addresses: ArcSwap<UpstreamAddresses>,
    next: AtomicUsize,
}

impl PinnedUpstream {
    pub fn new(host: impl Into<String>, port: u16, addresses: UpstreamAddresses) -> Self {
        Self {
            host: host.into(),
            port,
            addresses: ArcSwap::from_pointee(addresses),
            next: AtomicUsize::new(0),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The addresses currently in effect.
    pub fn addresses(&self) -> Arc<UpstreamAddresses> {
        self.addresses.load_full()
    }

    /// The address to dial for the next request, round-robin over the checked
    /// addresses, or why there is none.
    pub fn pick(&self) -> Result<SocketAddr, String> {
        match &**self.addresses.load() {
            UpstreamAddresses::Allowed(addresses) if !addresses.is_empty() => {
                let index = self.next.fetch_add(1, Ordering::Relaxed) % addresses.len();
                Ok(addresses[index])
            }
            UpstreamAddresses::Allowed(_) | UpstreamAddresses::Pending => Err(format!(
                "upstream {}:{} has not resolved to an allowed address yet",
                self.host, self.port
            )),
            UpstreamAddresses::Refused(reason) => Err(format!(
                "upstream {}:{} is not served: {reason}",
                self.host, self.port
            )),
        }
    }

    /// [`Self::pick`], skipping addresses in `avoid` (ones this request
    /// already failed to connect to) while another checked address is left.
    pub fn pick_avoiding(&self, avoid: &[String]) -> Result<SocketAddr, String> {
        let first = self.pick()?;
        if avoid.is_empty() {
            return Ok(first);
        }
        let addresses = self.addresses.load();
        let UpstreamAddresses::Allowed(addresses) = &**addresses else {
            return Ok(first);
        };
        let start = addresses.iter().position(|a| *a == first).unwrap_or(0);
        Ok((0..addresses.len())
            .map(|offset| addresses[(start + offset) % addresses.len()])
            .find(|candidate| !avoid.iter().any(|failed| *failed == candidate.to_string()))
            .unwrap_or(first))
    }

    /// The pin a reload creates for this name when the name could not be
    /// resolved during that reload: it starts from the addresses in effect
    /// here instead of from nothing, so an unrelated route change cannot take
    /// a working route down while the resolver is briefly unavailable.
    pub fn carried_over(&self) -> UpstreamAddresses {
        (*self.addresses.load_full()).clone()
    }

    /// Resolve and check the name again, and keep the result.
    pub async fn refresh(&self) {
        let verdict = resolve_upstream(&self.host, i32::from(self.port)).await;
        self.apply(verdict);
    }

    /// Record a check outcome directly, for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn apply_for_test(&self, verdict: Result<Vec<SocketAddr>, RouteUpstreamError>) {
        self.apply(verdict);
    }

    /// Keep the outcome of one check. A resolver failure keeps what is in
    /// effect, so a transient error does not take a working route down; a
    /// forbidden answer replaces it, so the route stops being served.
    fn apply(&self, verdict: Result<Vec<SocketAddr>, RouteUpstreamError>) {
        let previous = self.addresses.load_full();
        let next = match verdict {
            Ok(addresses) => UpstreamAddresses::Allowed(addresses),
            Err(error) if error.is_unresolvable() => {
                debug!(
                    "Custom route upstream {}:{} could not be resolved again; keeping {:?}: {}",
                    self.host, self.port, previous, error
                );
                return;
            }
            Err(error) => UpstreamAddresses::Refused(error.to_string()),
        };
        if *previous == next {
            return;
        }
        match (&*previous, &next) {
            (_, UpstreamAddresses::Refused(reason)) => warn!(
                "Custom route upstream {}:{} now resolves to a forbidden address; requests \
                 to it are refused until it resolves to an allowed one: {}",
                self.host, self.port, reason
            ),
            (UpstreamAddresses::Refused(_), UpstreamAddresses::Allowed(addresses)) => info!(
                "Custom route upstream {}:{} resolves to allowed addresses again: {:?}",
                self.host, self.port, addresses
            ),
            (_, _) => debug!(
                "Custom route upstream {}:{} now resolves to {:?}",
                self.host, self.port, next
            ),
        }
        self.addresses.store(Arc::new(next));
    }
}

/// Re-check `pins` every [`REFRESH_INTERVAL`] until none of them is in use
/// any more. Holds only weak references: once the route table that owns the
/// pins is replaced, the task ends at its next tick.
pub fn spawn_refresher(pins: Vec<Weak<PinnedUpstream>>) {
    spawn_refresher_every(pins, REFRESH_INTERVAL);
}

fn spawn_refresher_every(pins: Vec<Weak<PinnedUpstream>>, interval: Duration) {
    if pins.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The first tick completes at once; the pins were just checked.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let live: Vec<Arc<PinnedUpstream>> = pins.iter().filter_map(Weak::upgrade).collect();
            if live.is_empty() {
                return;
            }
            for chunk in live.chunks(REFRESH_CONCURRENCY) {
                let mut set = tokio::task::JoinSet::new();
                for pin in chunk {
                    let pin = pin.clone();
                    set.spawn(async move { pin.refresh().await });
                }
                while set.join_next().await.is_some() {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> SocketAddr {
        text.parse().expect("socket address")
    }

    #[test]
    fn requests_go_to_the_checked_addresses_in_turn() {
        let pin = PinnedUpstream::new(
            "upstream.example.net",
            443,
            UpstreamAddresses::Allowed(vec![address("8.8.8.8:443"), address("8.8.4.4:443")]),
        );
        assert_eq!(pin.pick(), Ok(address("8.8.8.8:443")));
        assert_eq!(pin.pick(), Ok(address("8.8.4.4:443")));
        assert_eq!(pin.pick(), Ok(address("8.8.8.8:443")));
    }

    #[test]
    fn nothing_is_dialled_before_the_name_resolves_or_after_it_is_refused() {
        let pending = PinnedUpstream::new("upstream.example.net", 80, UpstreamAddresses::Pending);
        assert!(pending.pick().is_err());
        let empty = PinnedUpstream::new(
            "upstream.example.net",
            80,
            UpstreamAddresses::Allowed(vec![]),
        );
        assert!(empty.pick().is_err());
        let refused = PinnedUpstream::new(
            "upstream.example.net",
            80,
            UpstreamAddresses::Refused("loopback".to_string()),
        );
        let error = refused.pick().expect_err("refused");
        assert!(error.contains("upstream.example.net:80"), "{error}");
    }

    /// The rebinding case: the name passed when the table loaded, then
    /// answers with a loopback address. The next check replaces the pinned
    /// public address, and the route is no longer dialled at all.
    #[tokio::test]
    async fn a_name_that_starts_resolving_to_an_internal_address_stops_being_served() {
        // `localhost` stands in for a name whose new answer is loopback: the
        // policy refuses it without needing the network.
        let pin = PinnedUpstream::new(
            "localhost",
            8080,
            UpstreamAddresses::Allowed(vec![address("8.8.8.8:8080")]),
        );
        assert_eq!(pin.pick(), Ok(address("8.8.8.8:8080")));
        pin.refresh().await;
        assert!(matches!(*pin.addresses(), UpstreamAddresses::Refused(_)));
        assert!(pin.pick().is_err());
    }

    #[test]
    fn a_resolver_failure_keeps_the_addresses_in_effect() {
        let pin = PinnedUpstream::new(
            "upstream.example.net",
            443,
            UpstreamAddresses::Allowed(vec![address("8.8.8.8:443")]),
        );
        pin.apply(Err(RouteUpstreamError::Unresolvable {
            host: "upstream.example.net".to_string(),
            detail: "timed out".to_string(),
        }));
        assert_eq!(pin.pick(), Ok(address("8.8.8.8:443")));

        pin.apply(Ok(vec![address("1.1.1.1:443")]));
        assert_eq!(pin.pick(), Ok(address("1.1.1.1:443")));
    }

    #[tokio::test]
    async fn the_refresher_rechecks_pins_and_stops_when_they_are_dropped() {
        let interval = Duration::from_millis(50);
        let pin = Arc::new(PinnedUpstream::new(
            "localhost",
            8080,
            UpstreamAddresses::Allowed(vec![address("8.8.8.8:8080")]),
        ));
        let weak = Arc::downgrade(&pin);
        spawn_refresher_every(vec![weak.clone()], interval);
        assert_eq!(
            pin.pick(),
            Ok(address("8.8.8.8:8080")),
            "not before the interval"
        );

        let mut refused = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if pin.pick().is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused, "the refresh refused the new answer");

        // Only weak references are held, so dropping the table's pin frees it.
        drop(pin);
        assert!(weak.upgrade().is_none());
    }
}
