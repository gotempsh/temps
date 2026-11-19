// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory route table for the edge proxy.
//!
//! Maps domain names to route information. Populated by polling the origin
//! control plane — no database required. Lookups are lock-free (`ArcSwap`).

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A single route entry received from the origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRoute {
    pub domain: String,
    /// Whether this is a static-file deployment (vs container-based).
    pub is_static: bool,
    /// Whether this domain uses wildcard matching (e.g. `*.localho.st`).
    #[serde(default)]
    pub is_wildcard: bool,
    /// Project ID on the origin (for debugging/logging).
    pub project_id: Option<i32>,
    /// Environment ID on the origin.
    pub environment_id: Option<i32>,
}

/// An encrypted TLS certificate bundle from the control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeCertBundle {
    pub domain: String,
    pub ciphertext: String,
    pub nonce: String,
    pub fingerprint: String,
}

/// Encrypted certificate payload in the edge routes response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeCertificates {
    pub ephemeral_public_key: String,
    pub bundles: Vec<EdgeCertBundle>,
}

/// Response from `GET /api/internal/edge/routes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRoutesResponse {
    pub routes: Vec<EdgeRoute>,
    /// Monotonic version counter — increments on route table changes.
    pub version: u64,
    /// Encrypted TLS certificates (present if edge node has a registered public key)
    #[serde(default)]
    pub certificates: Option<EdgeCertificates>,
}

/// Thread-safe in-memory route table with wildcard support.
///
/// Readers (one per request) take a lock-free `ArcSwap` snapshot; the sync
/// loop builds a whole new table and swaps it in.
pub struct EdgeRouteTable {
    inner: ArcSwap<RouteTableInner>,
}

#[derive(Default)]
struct RouteTableInner {
    /// Exact domain → route mapping.
    routes: HashMap<String, EdgeRoute>,
    /// Wildcard routes keyed by the domain they cover the subdomains of,
    /// e.g. `*.localho.st` → key = `localho.st`.
    wildcards: HashMap<String, EdgeRoute>,
    version: u64,
}

impl RouteTableInner {
    fn from_response(response: EdgeRoutesResponse) -> Self {
        let mut routes = HashMap::with_capacity(response.routes.len());
        let mut wildcards = HashMap::new();
        for route in response.routes {
            match route.domain.strip_prefix("*.") {
                Some(base_domain) if route.is_wildcard => {
                    wildcards.insert(base_domain.to_string(), route);
                }
                _ => {
                    routes.insert(route.domain.clone(), route);
                }
            }
        }
        Self {
            routes,
            wildcards,
            version: response.version,
        }
    }

    /// Exact match first, then a single-level wildcard: `a.example.com`
    /// matches `*.example.com`, but neither `example.com` nor
    /// `a.b.example.com` does.
    fn lookup(&self, domain: &str) -> Option<&EdgeRoute> {
        self.routes.get(domain).or_else(|| {
            let (_, parent) = domain.split_once('.')?;
            self.wildcards.get(parent)
        })
    }
}

/// Whether a table at `current` version should be replaced by `incoming`.
/// An empty table (version 0) accepts anything.
fn accepts_version(current: u64, incoming: u64) -> bool {
    current == 0 || incoming > current
}

impl Default for EdgeRouteTable {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgeRouteTable {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(RouteTableInner::default()),
        }
    }

    /// Look up a route by domain name (exact match, then wildcard).
    pub fn get(&self, domain: &str) -> Option<EdgeRoute> {
        self.inner.load().lookup(domain).cloned()
    }

    /// Check if a domain has a route (exact or wildcard).
    pub fn contains(&self, domain: &str) -> bool {
        self.inner.load().lookup(domain).is_some()
    }

    /// Replace the entire route table atomically.
    /// Only updates if the new version is higher than the current one.
    pub fn replace(&self, response: EdgeRoutesResponse) {
        if !accepts_version(self.inner.load().version, response.version) {
            return;
        }
        let next = Arc::new(RouteTableInner::from_response(response));
        // Re-check against whatever is current at swap time, so two
        // concurrent replacements can never install the older version.
        self.inner.rcu(|current| {
            if accepts_version(current.version, next.version) {
                Arc::clone(&next)
            } else {
                Arc::clone(current)
            }
        });
    }

    /// Current route table version.
    pub fn version(&self) -> u64 {
        self.inner.load().version
    }

    /// Number of routes in the table (exact + wildcard).
    pub fn len(&self) -> usize {
        let inner = self.inner.load();
        inner.routes.len() + inner.wildcards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return all known domains (for debugging/logging).
    pub fn domains(&self) -> Vec<String> {
        let inner = self.inner.load();
        let mut domains: Vec<String> = inner.routes.keys().cloned().collect();
        for route in inner.wildcards.values() {
            domains.push(route.domain.clone());
        }
        domains
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_routes() -> EdgeRoutesResponse {
        EdgeRoutesResponse {
            routes: vec![
                EdgeRoute {
                    domain: "app.example.com".to_string(),
                    is_static: false,
                    is_wildcard: false,
                    project_id: Some(1),
                    environment_id: Some(10),
                },
                EdgeRoute {
                    domain: "docs.example.com".to_string(),
                    is_static: true,
                    is_wildcard: false,
                    project_id: Some(2),
                    environment_id: Some(20),
                },
            ],
            version: 1,
            certificates: None,
        }
    }

    #[test]
    fn test_new_table_is_empty() {
        let table = EdgeRouteTable::new();
        assert!(table.is_empty());
        assert_eq!(table.version(), 0);
    }

    #[test]
    fn test_replace_and_get() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());

        assert_eq!(table.len(), 2);
        assert_eq!(table.version(), 1);

        let route = table.get("app.example.com").unwrap();
        assert!(!route.is_static);
        assert_eq!(route.project_id, Some(1));

        let route = table.get("docs.example.com").unwrap();
        assert!(route.is_static);
    }

    #[test]
    fn test_get_missing_domain() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());
        assert!(table.get("unknown.com").is_none());
    }

    #[test]
    fn test_replace_skips_older_version() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());
        assert_eq!(table.len(), 2);

        // Try replacing with an older version — should be ignored
        let old = EdgeRoutesResponse {
            routes: vec![],
            version: 0,
            certificates: None,
        };
        table.replace(old);
        assert_eq!(table.len(), 2); // unchanged
    }

    #[test]
    fn test_replace_accepts_newer_version() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());

        let new = EdgeRoutesResponse {
            routes: vec![EdgeRoute {
                domain: "new.example.com".to_string(),
                is_static: true,
                is_wildcard: false,
                project_id: Some(3),
                environment_id: None,
            }],
            version: 2,
            certificates: None,
        };
        table.replace(new);
        assert_eq!(table.len(), 1);
        assert!(table.get("new.example.com").is_some());
        assert!(table.get("app.example.com").is_none()); // old routes gone
    }

    #[test]
    fn test_contains() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());
        assert!(table.contains("app.example.com"));
        assert!(!table.contains("nope.com"));
    }

    #[test]
    fn test_domains() {
        let table = EdgeRouteTable::new();
        table.replace(sample_routes());
        let mut domains = table.domains();
        domains.sort();
        assert_eq!(domains, vec!["app.example.com", "docs.example.com"]);
    }

    // ── Wildcard tests ──

    fn routes_with_wildcards() -> EdgeRoutesResponse {
        EdgeRoutesResponse {
            routes: vec![
                EdgeRoute {
                    domain: "app.example.com".to_string(),
                    is_static: false,
                    is_wildcard: false,
                    project_id: Some(1),
                    environment_id: Some(10),
                },
                EdgeRoute {
                    domain: "*.localho.st".to_string(),
                    is_static: false,
                    is_wildcard: true,
                    project_id: None,
                    environment_id: None,
                },
                EdgeRoute {
                    domain: "*.example.org".to_string(),
                    is_static: false,
                    is_wildcard: true,
                    project_id: None,
                    environment_id: None,
                },
            ],
            version: 5,
            certificates: None,
        }
    }

    #[test]
    fn test_wildcard_match() {
        let table = EdgeRouteTable::new();
        table.replace(routes_with_wildcards());

        // Exact match still works
        assert!(table.contains("app.example.com"));

        // Wildcard matches single-level subdomains
        assert!(table.contains("my-app-production.localho.st"));
        assert!(table.contains("temps-landing-new-production.localho.st"));
        assert!(table.contains("anything.localho.st"));
        assert!(table.contains("sub.example.org"));

        // Wildcard does NOT match the base domain itself
        assert!(!table.contains("localho.st"));

        // Wildcard does NOT match multi-level subdomains
        assert!(!table.contains("a.b.localho.st"));

        // Unknown domains still fail
        assert!(!table.contains("nope.com"));
    }

    #[test]
    fn test_wildcard_get() {
        let table = EdgeRouteTable::new();
        table.replace(routes_with_wildcards());

        let route = table.get("my-app.localho.st");
        assert!(route.is_some());
        let route = route.unwrap();
        assert_eq!(route.domain, "*.localho.st");
        assert!(route.is_wildcard);
    }

    #[test]
    fn test_wildcard_count() {
        let table = EdgeRouteTable::new();
        table.replace(routes_with_wildcards());
        // 1 exact + 2 wildcards
        assert_eq!(table.len(), 3);
    }

    #[test]
    fn test_single_label_no_wildcard_match() {
        let table = EdgeRouteTable::new();
        table.replace(routes_with_wildcards());
        assert!(!table.contains("localhost"));
    }

    #[test]
    fn test_non_wildcard_star_domain_is_exact() {
        // A `*.` domain not flagged as wildcard is stored as an exact route,
        // as before.
        let table = EdgeRouteTable::new();
        table.replace(EdgeRoutesResponse {
            routes: vec![EdgeRoute {
                domain: "*.literal.test".to_string(),
                is_static: false,
                is_wildcard: false,
                project_id: None,
                environment_id: None,
            }],
            version: 1,
            certificates: None,
        });
        assert!(table.contains("*.literal.test"));
        assert!(!table.contains("a.literal.test"));
    }

    #[test]
    fn test_concurrent_reads_during_replacement() {
        let table = Arc::new(EdgeRouteTable::new());
        table.replace(sample_routes());

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let table = Arc::clone(&table);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        // Every version this test installs routes this host.
                        assert!(table.contains("app.example.com"));
                    }
                })
            })
            .collect();

        for version in 2..200 {
            let mut routes = sample_routes();
            routes.version = version;
            table.replace(routes);
        }
        for reader in readers {
            reader.join().unwrap();
        }
        assert_eq!(table.version(), 199);
    }
}
