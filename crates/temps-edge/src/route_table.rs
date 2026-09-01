// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory route table for the edge proxy.
//!
//! Maps domain names to route information. Populated by polling the origin
//! control plane — no database required. Thread-safe via `RwLock`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

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
pub struct EdgeRouteTable {
    inner: RwLock<RouteTableInner>,
}

struct RouteTableInner {
    /// Exact domain → route mapping.
    routes: HashMap<String, EdgeRoute>,
    /// Wildcard patterns stored as reversed base domain → route.
    /// e.g. `*.localho.st` → key = `st.localho`
    wildcards: HashMap<String, EdgeRoute>,
    version: u64,
}

/// Reverse domain labels: `api.example.com` → `com.example.api`
fn reverse_domain(domain: &str) -> String {
    domain.split('.').rev().collect::<Vec<_>>().join(".")
}

impl Default for EdgeRouteTable {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgeRouteTable {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(RouteTableInner {
                routes: HashMap::new(),
                wildcards: HashMap::new(),
                version: 0,
            }),
        }
    }

    /// Look up a route by domain name (exact match, then wildcard).
    pub fn get(&self, domain: &str) -> Option<EdgeRoute> {
        let inner = self.inner.read().unwrap();
        if let Some(route) = inner.routes.get(domain) {
            return Some(route.clone());
        }
        // Try wildcard: reverse domain, strip last label, look up
        let reversed = reverse_domain(domain);
        if let Some(dot_pos) = reversed.rfind('.') {
            let base_key = &reversed[..dot_pos];
            inner.wildcards.get(base_key).cloned()
        } else {
            None
        }
    }

    /// Check if a domain has a route (exact or wildcard).
    pub fn contains(&self, domain: &str) -> bool {
        let inner = self.inner.read().unwrap();
        if inner.routes.contains_key(domain) {
            return true;
        }
        let reversed = reverse_domain(domain);
        if let Some(dot_pos) = reversed.rfind('.') {
            let base_key = &reversed[..dot_pos];
            inner.wildcards.contains_key(base_key)
        } else {
            false
        }
    }

    /// Replace the entire route table atomically.
    /// Only updates if the new version is higher than the current one.
    pub fn replace(&self, response: EdgeRoutesResponse) {
        let mut inner = self.inner.write().unwrap();
        if response.version <= inner.version && inner.version > 0 {
            return;
        }
        let mut routes = HashMap::with_capacity(response.routes.len());
        let mut wildcards = HashMap::new();
        for route in response.routes {
            if route.is_wildcard && route.domain.starts_with("*.") {
                let base_domain = &route.domain[2..];
                let reversed_key = reverse_domain(base_domain);
                wildcards.insert(reversed_key, route);
            } else {
                routes.insert(route.domain.clone(), route);
            }
        }
        inner.routes = routes;
        inner.wildcards = wildcards;
        inner.version = response.version;
    }

    /// Current route table version.
    pub fn version(&self) -> u64 {
        let inner = self.inner.read().unwrap();
        inner.version
    }

    /// Number of routes in the table (exact + wildcard).
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.routes.len() + inner.wildcards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return all known domains (for debugging/logging).
    pub fn domains(&self) -> Vec<String> {
        let inner = self.inner.read().unwrap();
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
}
