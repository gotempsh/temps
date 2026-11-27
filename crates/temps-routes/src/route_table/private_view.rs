// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The control plane's private view (ADR-115 D3).
//!
//! A private address is withdrawn from every map a public connection is
//! matched against (ADR 064 D5). When node `0`, the control plane, serves
//! one on its listener, the proxy still needs a route for it, but only for a
//! connection that arrived on the consented private address from inside its
//! range. That route lives here: a second [`CachedPeerTable`] whose maps hold
//! node `0`'s listener addresses and nothing else, republished from every
//! snapshot the main table stores. The proxy reads it only on the private
//! path, through the same resolvers as the main table, so a private request
//! is routed exactly like a public one once it is admitted.

use std::collections::HashMap;
use std::sync::Arc;

use super::{CachedPeerTable, LegacyRouteTable, PrivateAddress, PrivateRoute, RouteTableSnapshot};

/// Node `0`'s private addresses served on its listener, from `snapshot`, as
/// a snapshot of their own. The console's and the public ingress hostname
/// stay reserved here too (D5), so a private address equal to either is
/// never served.
fn control_plane_private_snapshot(snapshot: &RouteTableSnapshot) -> RouteTableSnapshot {
    let private: HashMap<String, PrivateRoute> = snapshot
        .private
        .iter()
        .filter(|(_, private)| {
            private.address.node_id == Some(temps_core::CONTROL_PLANE_NODE_ID)
                && private.address.on_listener()
                && private.address.range.is_some()
        })
        // Keyed by the normalized host, so the request path finds one with
        // a single hash lookup, an unknown host included.
        .map(|(host, private)| (normalize_host(host), private.clone()))
        .collect();
    let exact = private
        .iter()
        .map(|(host, private)| (host.clone(), private.route.clone()))
        .collect();
    let path_routes = if private.is_empty() {
        HashMap::new()
    } else {
        snapshot.path_routes.clone()
    };
    RouteTableSnapshot {
        owned_hosts: private.keys().cloned().collect(),
        legacy: LegacyRouteTable {
            exact,
            reserved_console_host: snapshot.legacy.reserved_console_host.clone(),
            reserved_ingress_host: snapshot.legacy.reserved_ingress_host.clone(),
            ..LegacyRouteTable::default()
        },
        path_routes,
        ingress: Arc::clone(&snapshot.ingress),
        private,
        ..RouteTableSnapshot::default()
    }
}

/// A host as the private view keys it: lower case, without a trailing dot.
fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

impl CachedPeerTable {
    /// The control plane's private view (ADR-115 D3), created on first use
    /// from the current snapshot and kept current by every reload after it.
    pub fn private_view(&self) -> Arc<CachedPeerTable> {
        let view = self.private_view.get_or_init(|| {
            Arc::new(CachedPeerTable::new_with_runtime_context(
                Arc::clone(&self.db),
                Arc::clone(&self.runtime_context),
            ))
        });
        let view = Arc::clone(view);
        self.publish_private_view();
        view
    }

    /// Republish the private view from the current snapshot, when one was
    /// asked for. One pass over the private addresses; never on the request
    /// path.
    pub(super) fn publish_private_view(&self) {
        let Some(view) = self.private_view.get() else {
            return;
        };
        let snapshot = self.route_snapshot.load();
        view.route_snapshot
            .store(Arc::new(control_plane_private_snapshot(&snapshot)));
        view.loaded
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The private route at `host` in this view: the route and the range a
    /// visitor must come from, both from one snapshot, so a reload between
    /// the range check and the backend choice can never pair one host's
    /// range with a replacement route. `None` for a host the view does not
    /// serve, and for the reserved console and ingress hostnames (D5). One
    /// hash lookup.
    pub fn private_route_for(&self, host: &str) -> Option<PrivateRoute> {
        let snapshot = self.route_snapshot.load();
        if snapshot.private.is_empty() {
            return None;
        }
        let host = normalize_host(host);
        if snapshot.legacy.is_reserved(&host) {
            return None;
        }
        snapshot.private.get(&host).cloned()
    }

    /// The private address at `host` in this view, with the range a visitor
    /// must come from. `None` for a host the view does not serve.
    pub fn private_address_for(&self, host: &str) -> Option<PrivateAddress> {
        self.private_route_for(host).map(|private| private.address)
    }

    /// Hosts node `0` serves on its listener, for the inside checks.
    pub fn control_plane_private_hosts(&self) -> Vec<(String, PrivateAddress)> {
        let snapshot = self.route_snapshot.load();
        snapshot
            .private
            .iter()
            .filter(|(_, private)| {
                private.address.node_id == Some(temps_core::CONTROL_PLANE_NODE_ID)
                    && private.address.on_listener()
            })
            .map(|(host, private)| (host.clone(), private.address.clone()))
            .collect()
    }

    /// Make `host` a private address for cross-crate tests without database
    /// setup, as the loader would after withdrawing it.
    #[doc(hidden)]
    pub fn insert_private_route_for_test(&self, host: &str, route: PrivateRoute) {
        let mut snapshot = (*self.route_snapshot.load_full()).clone();
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        snapshot.legacy.private_hosts.insert(host.clone());
        snapshot.private.insert(host, route);
        self.route_snapshot.store(Arc::new(snapshot));
        self.publish_private_view();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BackendType, RouteInfo};
    use super::*;

    fn route(path: &str) -> RouteInfo {
        RouteInfo {
            backend: BackendType::StaticDir {
                path: path.to_string(),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
            cert_eligible: false,
        }
    }

    fn address(node_id: Option<i32>, via: &str) -> PrivateAddress {
        PrivateAddress {
            address_id: "env:1".into(),
            range: Some("100.64.0.0/10".into()),
            node_id,
            listener: Some("100.101.102.103".into()),
            via: Some(via.into()),
            principals: None,
        }
    }

    fn table() -> CachedPeerTable {
        CachedPeerTable::new(Arc::new(sea_orm::DatabaseConnection::Disconnected))
    }

    #[test]
    fn the_view_holds_only_node_zeros_listener_addresses() {
        let table = table();
        table.reserve_hostname_for_test("console.example.com");
        let view = table.private_view();
        assert!(view.get_route("app.internal.example.com").is_none());

        table.insert_private_route_for_test(
            "App.Internal.Example.com",
            PrivateRoute {
                route: route("/srv/app"),
                address: address(Some(0), "listener"),
            },
        );
        for (host, private) in [
            ("worker.internal.example.com", address(Some(3), "listener")),
            (
                "serve.internal.example.com",
                address(Some(0), "tailscale_serve"),
            ),
            ("nobody.internal.example.com", address(None, "listener")),
            ("console.example.com", address(Some(0), "listener")),
        ] {
            table.insert_private_route_for_test(
                host,
                PrivateRoute {
                    route: route("/srv/other"),
                    address: private,
                },
            );
        }

        // The public table serves none of them.
        assert!(table.get_route("app.internal.example.com").is_none());
        assert!(table
            .get_route_by_host("app.internal.example.com")
            .is_none());

        // The view serves node 0's listener address, by every lookup.
        assert!(view.get_route("app.internal.example.com").is_some());
        assert!(view.get_route_by_host("app.internal.example.com").is_some());
        assert!(view
            .resolve_route_for_sni("app.internal.example.com")
            .is_some());
        assert_eq!(
            view.private_address_for("APP.internal.example.com.")
                .and_then(|a| a.range),
            Some("100.64.0.0/10".to_string())
        );
        // Not another worker's, not a Serve address, not one nobody serves,
        // and never the console's host.
        for host in [
            "worker.internal.example.com",
            "serve.internal.example.com",
            "nobody.internal.example.com",
            "console.example.com",
        ] {
            assert!(view.get_route(host).is_none(), "{host}");
            assert!(view.get_route_by_host(host).is_none(), "{host}");
        }
        // Nothing public leaks into the view.
        assert!(view.get_route("www.example.com").is_none());
        assert_eq!(table.control_plane_private_hosts().len(), 2);
    }

    #[test]
    fn a_view_asked_for_later_starts_from_the_current_snapshot() {
        let table = table();
        table.insert_private_route_for_test(
            "app.internal.example.com",
            PrivateRoute {
                route: route("/srv/app"),
                address: address(Some(0), "listener"),
            },
        );
        let view = table.private_view();
        assert!(view.get_route("app.internal.example.com").is_some());
        // The same view is handed out again.
        assert!(Arc::ptr_eq(&view, &table.private_view()));
    }

    #[test]
    fn an_address_without_a_range_admits_nobody() {
        let table = table();
        let mut unbounded = address(Some(0), "listener");
        unbounded.range = None;
        table.insert_private_route_for_test(
            "app.internal.example.com",
            PrivateRoute {
                route: route("/srv/app"),
                address: unbounded,
            },
        );
        assert!(table
            .private_view()
            .get_route("app.internal.example.com")
            .is_none());
    }

    /// The proxy checks a private route's range and sends the request to
    /// that same route: both come from one lookup, so a reload that replaces
    /// the host's route and narrows its range cannot pair the old range
    /// with the new backend.
    #[test]
    fn a_private_route_and_its_range_come_from_one_snapshot() {
        let table = table();
        table.reserve_hostname_for_test("console.example.com");
        let view = table.private_view();
        table.insert_private_route_for_test(
            "app.internal.example.com",
            PrivateRoute {
                route: route("/srv/old"),
                address: address(Some(0), "listener"),
            },
        );
        let first = view
            .private_route_for("App.Internal.Example.com.")
            .expect("served");
        assert_eq!(first.route.static_dir(), Some("/srv/old"));
        assert_eq!(first.address.range.as_deref(), Some("100.64.0.0/10"));

        let mut narrowed = address(Some(0), "listener");
        narrowed.range = Some("100.64.0.0/24".into());
        table.insert_private_route_for_test(
            "app.internal.example.com",
            PrivateRoute {
                route: route("/srv/new"),
                address: narrowed,
            },
        );
        let second = view
            .private_route_for("app.internal.example.com")
            .expect("served");
        assert_eq!(second.route.static_dir(), Some("/srv/new"));
        assert_eq!(second.address.range.as_deref(), Some("100.64.0.0/24"));

        // Unknown and reserved hosts are not served.
        assert!(view
            .private_route_for("other.internal.example.com")
            .is_none());
        table.insert_private_route_for_test(
            "console.example.com",
            PrivateRoute {
                route: route("/srv/console"),
                address: address(Some(0), "listener"),
            },
        );
        assert!(view.private_route_for("console.example.com").is_none());
    }
}
