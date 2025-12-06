//! Efficient wildcard pattern matcher for DNS-style domain matching
//!
//! This module provides wildcard matching following DNS/Cloudflare conventions:
//! - `*.example.com` matches `api.example.com` ✓
//! - `*.example.com` does NOT match `sub.api.example.com` ✗ (multi-level)
//! - `*.example.com` does NOT match `example.com` ✗ (no subdomain)
//!
//! The implementation uses reversed domain storage for O(1) lookup after domain reversal.

use super::RouteInfo;
use std::collections::HashMap;

/// Efficient wildcard pattern matcher using reversed domain storage
///
/// Wildcards like `*.example.com` are stored as reversed base domains:
/// - Pattern: `*.example.com` → Key: `com.example`
/// - To match `api.example.com`:
///   1. Reverse: `com.example.api`
///   2. Strip first label: `com.example`
///   3. Lookup in HashMap → O(1)
#[derive(Debug, Default)]
pub struct WildcardMatcher {
    /// Map of reversed base domain (without `*.` prefix) -> RouteInfo
    /// e.g., `*.example.com` → key = `com.example`
    patterns: HashMap<String, RouteInfo>,
}

impl WildcardMatcher {
    /// Create a new empty WildcardMatcher
    pub fn new() -> Self {
        Self {
            patterns: HashMap::new(),
        }
    }

    /// Add a wildcard pattern (must start with `*.`)
    ///
    /// # Arguments
    /// * `pattern` - The wildcard pattern (e.g., `*.example.com`)
    /// * `route` - The route information for this pattern
    ///
    /// # Returns
    /// * `true` if the pattern was added
    /// * `false` if the pattern doesn't start with `*.` or contains additional wildcards
    pub fn insert(&mut self, pattern: &str, route: RouteInfo) -> bool {
        if !pattern.starts_with("*.") {
            return false;
        }

        // Extract base domain (without `*.`) and verify no additional wildcards
        let base_domain = &pattern[2..]; // Remove `*.`

        // Reject patterns with additional wildcards (e.g., `*.*.example.com`)
        if base_domain.contains('*') {
            return false;
        }

        // Reject empty base domain
        if base_domain.is_empty() {
            return false;
        }

        let reversed_key = reverse_domain(base_domain);

        self.patterns.insert(reversed_key, route);
        true
    }

    /// Match a domain against wildcard patterns
    ///
    /// # Arguments
    /// * `domain` - The domain to match (e.g., `api.example.com`)
    ///
    /// # Returns
    /// The matching RouteInfo if found, None otherwise
    ///
    /// # Algorithm
    /// 1. Reverse the domain: `api.example.com` → `com.example.api`
    /// 2. Find the LAST `.` to strip the last label (which was the subdomain)
    /// 3. Take everything BEFORE the last `.`: `com.example`
    /// 4. Look up in the HashMap
    pub fn match_domain(&self, domain: &str) -> Option<&RouteInfo> {
        // Reverse the domain
        let reversed = reverse_domain(domain);

        // Find the LAST `.` to strip the last reversed label (which was the subdomain)
        // For `com.example.api`, we want `com.example`
        if let Some(dot_pos) = reversed.rfind('.') {
            let base_key = &reversed[..dot_pos];
            self.patterns.get(base_key)
        } else {
            // No dot found, meaning single-label domain like `localhost`
            // Wildcards don't match single-label domains
            None
        }
    }

    /// Remove a wildcard pattern
    ///
    /// # Arguments
    /// * `pattern` - The wildcard pattern to remove (e.g., `*.example.com`)
    ///
    /// # Returns
    /// The removed RouteInfo if found
    pub fn remove(&mut self, pattern: &str) -> Option<RouteInfo> {
        if !pattern.starts_with("*.") {
            return None;
        }

        let base_domain = &pattern[2..];
        let reversed_key = reverse_domain(base_domain);

        self.patterns.remove(&reversed_key)
    }

    /// Check if the matcher is empty
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Get the number of patterns in the matcher
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Clear all patterns
    pub fn clear(&mut self) {
        self.patterns.clear();
    }
}

/// Reverse domain labels
///
/// `api.example.com` → `com.example.api`
fn reverse_domain(domain: &str) -> String {
    domain.split('.').rev().collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_table::BackendType;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    fn create_test_route(addr: &str) -> RouteInfo {
        RouteInfo {
            backend: BackendType::Upstream {
                addresses: vec![addr.to_string()],
                round_robin_counter: Arc::new(AtomicUsize::new(0)),
            },
            redirect_to: None,
            status_code: None,
            project: None,
            environment: None,
            deployment: None,
        }
    }

    #[test]
    fn test_reverse_domain() {
        assert_eq!(reverse_domain("api.example.com"), "com.example.api");
        assert_eq!(reverse_domain("example.com"), "com.example");
        assert_eq!(reverse_domain("localhost"), "localhost");
        assert_eq!(reverse_domain("a.b.c.d"), "d.c.b.a");
    }

    #[test]
    fn test_wildcard_insert_and_match() {
        let mut matcher = WildcardMatcher::new();

        // Insert a wildcard pattern
        let route = create_test_route("192.168.1.100:8080");
        assert!(matcher.insert("*.example.com", route));

        // Should match single-level subdomains
        assert!(matcher.match_domain("api.example.com").is_some());
        assert!(matcher.match_domain("www.example.com").is_some());
        assert!(matcher.match_domain("staging.example.com").is_some());

        // Should NOT match multi-level subdomains
        assert!(matcher.match_domain("sub.api.example.com").is_none());
        assert!(matcher.match_domain("a.b.example.com").is_none());

        // Should NOT match the base domain itself
        assert!(matcher.match_domain("example.com").is_none());

        // Should NOT match different domains
        assert!(matcher.match_domain("api.other.com").is_none());
    }

    #[test]
    fn test_wildcard_insert_invalid_pattern() {
        let mut matcher = WildcardMatcher::new();
        let route = create_test_route("127.0.0.1:8080");

        // Non-wildcard patterns should not be inserted
        assert!(!matcher.insert("example.com", route.clone()));
        assert!(!matcher.insert("api.example.com", route.clone()));
        assert!(!matcher.insert("*.*.example.com", route)); // Not a valid single wildcard

        assert!(matcher.is_empty());
    }

    #[test]
    fn test_wildcard_remove() {
        let mut matcher = WildcardMatcher::new();
        let route = create_test_route("192.168.1.100:8080");

        matcher.insert("*.example.com", route);
        assert_eq!(matcher.len(), 1);

        // Remove the pattern
        let removed = matcher.remove("*.example.com");
        assert!(removed.is_some());
        assert!(matcher.is_empty());

        // Should no longer match
        assert!(matcher.match_domain("api.example.com").is_none());
    }

    #[test]
    fn test_wildcard_remove_invalid() {
        let mut matcher = WildcardMatcher::new();
        let route = create_test_route("192.168.1.100:8080");

        matcher.insert("*.example.com", route);

        // Removing non-wildcard patterns should return None
        assert!(matcher.remove("example.com").is_none());
        assert!(matcher.remove("api.example.com").is_none());

        // Original pattern should still exist
        assert_eq!(matcher.len(), 1);
    }

    #[test]
    fn test_multiple_wildcards() {
        let mut matcher = WildcardMatcher::new();

        matcher.insert("*.example.com", create_test_route("10.0.0.1:8080"));
        matcher.insert("*.other.com", create_test_route("10.0.0.2:8080"));
        matcher.insert("*.example.org", create_test_route("10.0.0.3:8080"));

        assert_eq!(matcher.len(), 3);

        // Each should match its own pattern
        let route1 = matcher.match_domain("api.example.com").unwrap();
        assert_eq!(route1.get_backend_addr(), "10.0.0.1:8080");

        let route2 = matcher.match_domain("api.other.com").unwrap();
        assert_eq!(route2.get_backend_addr(), "10.0.0.2:8080");

        let route3 = matcher.match_domain("api.example.org").unwrap();
        assert_eq!(route3.get_backend_addr(), "10.0.0.3:8080");
    }

    #[test]
    fn test_single_label_domain() {
        let mut matcher = WildcardMatcher::new();
        matcher.insert("*.example.com", create_test_route("10.0.0.1:8080"));

        // Single-label domains should never match wildcards
        assert!(matcher.match_domain("localhost").is_none());
        assert!(matcher.match_domain("myhost").is_none());
    }

    #[test]
    fn test_wildcard_clear() {
        let mut matcher = WildcardMatcher::new();
        matcher.insert("*.example.com", create_test_route("10.0.0.1:8080"));
        matcher.insert("*.other.com", create_test_route("10.0.0.2:8080"));

        assert_eq!(matcher.len(), 2);

        matcher.clear();

        assert!(matcher.is_empty());
        assert!(matcher.match_domain("api.example.com").is_none());
    }

    #[test]
    fn test_edge_cases() {
        let mut matcher = WildcardMatcher::new();
        matcher.insert("*.a.b.c.d", create_test_route("10.0.0.1:8080"));

        // Should match one level up
        assert!(matcher.match_domain("x.a.b.c.d").is_some());

        // Should NOT match multiple levels
        assert!(matcher.match_domain("y.x.a.b.c.d").is_none());

        // Should NOT match base domain
        assert!(matcher.match_domain("a.b.c.d").is_none());
    }

    #[test]
    fn test_subdomain_boundary() {
        let mut matcher = WildcardMatcher::new();
        matcher.insert("*.co.uk", create_test_route("10.0.0.1:8080"));
        matcher.insert("*.example.co.uk", create_test_route("10.0.0.2:8080"));

        // *.co.uk should match any.co.uk
        let route1 = matcher.match_domain("example.co.uk").unwrap();
        assert_eq!(route1.get_backend_addr(), "10.0.0.1:8080");

        // *.example.co.uk should match api.example.co.uk
        let route2 = matcher.match_domain("api.example.co.uk").unwrap();
        assert_eq!(route2.get_backend_addr(), "10.0.0.2:8080");

        // www.example.co.uk should also match *.example.co.uk
        let route3 = matcher.match_domain("www.example.co.uk").unwrap();
        assert_eq!(route3.get_backend_addr(), "10.0.0.2:8080");
    }
}
