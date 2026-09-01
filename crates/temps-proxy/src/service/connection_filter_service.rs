// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! TCP-level connection filtering using Pingora 0.7.0's ConnectionFilter trait.
//!
//! This filter operates at the TCP layer, rejecting blocked IPs before any TLS
//! handshake or HTTP processing occurs. This is significantly more efficient than
//! the HTTP-layer IP blocking in `early_request_filter()` because:
//! - No TLS handshake overhead for blocked IPs
//! - No HTTP parsing overhead
//! - Minimal resource consumption per blocked connection
//!
//! The filter maintains an in-memory cache of blocked IP ranges that is refreshed
//! periodically from the database to avoid per-connection database queries.

use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_database::DbConnection;
use tracing::{debug, error, warn};

use pingora_core::listeners::ConnectionFilter;

/// How often to refresh the blocked IP cache from the database
const CACHE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Cached IP blocklist for TCP-level connection filtering.
///
/// This struct maintains an in-memory set of blocked IPs/CIDRs that is
/// periodically refreshed from the database. The cache ensures O(1) lookups
/// per connection without hitting the database.
#[derive(Debug)]
pub struct TcpConnectionFilter {
    /// The set of blocked individual IPs (exact matches)
    blocked_ips: Arc<RwLock<HashSet<IpAddr>>>,
    /// The set of blocked CIDR ranges stored as (network, prefix_len) tuples
    blocked_cidrs: Arc<RwLock<Vec<(IpAddr, u8)>>>,
    /// When the cache was last refreshed
    last_refresh: Arc<RwLock<Instant>>,
    /// Database connection for refreshing the cache
    db: Arc<DbConnection>,
}

impl TcpConnectionFilter {
    /// Create a new TcpConnectionFilter.
    ///
    /// The cache starts empty and is lazily populated on the first `should_accept` call.
    /// This avoids calling `tokio::spawn` from the constructor, which would panic in
    /// Pingora's synchronous startup context (no Tokio runtime available).
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self {
            blocked_ips: Arc::new(RwLock::new(HashSet::new())),
            blocked_cidrs: Arc::new(RwLock::new(Vec::new())),
            // Set to well in the past so first should_accept triggers an immediate refresh
            last_refresh: Arc::new(RwLock::new(Instant::now() - CACHE_REFRESH_INTERVAL * 2)),
            db,
        }
    }

    /// Refresh the blocked IP cache from the database
    async fn refresh_cache(&self) {
        use sea_orm::{ConnectionTrait, Statement};

        let sql = "SELECT ip_address::text FROM ip_access_control WHERE action = 'block'";

        match self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql.to_string(),
            ))
            .await
        {
            Ok(rows) => {
                let mut new_ips = HashSet::new();
                let mut new_cidrs = Vec::new();

                for row in rows {
                    let ip_str: String = match row.try_get("", "ip_address") {
                        Ok(ip) => ip,
                        Err(e) => {
                            warn!("Failed to parse IP from database row: {}", e);
                            continue;
                        }
                    };

                    if ip_str.contains('/') {
                        // CIDR notation
                        if let Some((ip, prefix)) = parse_cidr(&ip_str) {
                            new_cidrs.push((ip, prefix));
                        }
                    } else {
                        // Single IP
                        if let Ok(ip) = ip_str.parse::<IpAddr>() {
                            new_ips.insert(ip);
                        } else {
                            warn!("Invalid IP address in blocklist: {}", ip_str);
                        }
                    }
                }

                let ip_count = new_ips.len();
                let cidr_count = new_cidrs.len();

                *self.blocked_ips.write() = new_ips;
                *self.blocked_cidrs.write() = new_cidrs;
                *self.last_refresh.write() = Instant::now();

                debug!(
                    "Refreshed TCP connection filter cache: {} IPs, {} CIDRs",
                    ip_count, cidr_count
                );
            }
            Err(e) => {
                error!("Failed to refresh TCP connection filter cache: {}", e);
                // Don't update last_refresh so we retry sooner
            }
        }
    }

    /// Check if an IP address is blocked (exact match or CIDR containment)
    fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        // Check exact match first (O(1))
        if self.blocked_ips.read().contains(ip) {
            return true;
        }

        // Check CIDR ranges
        let cidrs = self.blocked_cidrs.read();
        for (network, prefix_len) in cidrs.iter() {
            if ip_in_cidr(ip, network, *prefix_len) {
                return true;
            }
        }

        false
    }

    /// Check if cache needs refresh and trigger it if needed
    fn maybe_refresh(&self) {
        let needs_refresh = {
            let last = self.last_refresh.read();
            last.elapsed() > CACHE_REFRESH_INTERVAL
        };

        if needs_refresh {
            let blocked_ips = self.blocked_ips.clone();
            let blocked_cidrs = self.blocked_cidrs.clone();
            let last_refresh = self.last_refresh.clone();
            let db = self.db.clone();

            tokio::spawn(async move {
                let temp_filter = TcpConnectionFilter {
                    blocked_ips,
                    blocked_cidrs,
                    last_refresh,
                    db,
                };
                temp_filter.refresh_cache().await;
            });
        }
    }
}

#[async_trait]
impl ConnectionFilter for TcpConnectionFilter {
    async fn should_accept(&self, addr: Option<&SocketAddr>) -> bool {
        // Trigger background refresh if needed
        self.maybe_refresh();

        let Some(addr) = addr else {
            return true; // Accept if no address available
        };

        let ip = addr.ip();

        if self.is_ip_blocked(&ip) {
            warn!(
                "TCP connection filter: rejected connection from blocked IP {}",
                ip
            );
            return false;
        }

        true
    }
}

/// Parse a CIDR string like "192.168.1.0/24" into (IpAddr, prefix_length)
fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return None;
    }

    let ip = parts[0].parse::<IpAddr>().ok()?;
    let prefix = parts[1].parse::<u8>().ok()?;

    // Validate prefix length
    match ip {
        IpAddr::V4(_) if prefix > 32 => return None,
        IpAddr::V6(_) if prefix > 128 => return None,
        _ => {}
    }

    Some((ip, prefix))
}

/// Check if an IP address falls within a CIDR range
fn ip_in_cidr(ip: &IpAddr, network: &IpAddr, prefix_len: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            if prefix_len == 0 {
                return true;
            }
            let ip_bits = u32::from(*ip);
            let net_bits = u32::from(*net);
            let mask = u32::MAX << (32 - prefix_len);
            (ip_bits & mask) == (net_bits & mask)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            if prefix_len == 0 {
                return true;
            }
            let ip_bits = u128::from(*ip);
            let net_bits = u128::from(*net);
            let mask = u128::MAX << (128 - prefix_len);
            (ip_bits & mask) == (net_bits & mask)
        }
        _ => false, // IPv4 vs IPv6 mismatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_parse_cidr_valid() {
        let result = parse_cidr("192.168.1.0/24");
        assert!(result.is_some());
        let (ip, prefix) = result.unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0)));
        assert_eq!(prefix, 24);
    }

    #[test]
    fn test_parse_cidr_single_host() {
        let result = parse_cidr("10.0.0.1/32");
        assert!(result.is_some());
        let (ip, prefix) = result.unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(prefix, 32);
    }

    #[test]
    fn test_parse_cidr_invalid_prefix() {
        assert!(parse_cidr("192.168.1.0/33").is_none());
    }

    #[test]
    fn test_parse_cidr_invalid_format() {
        assert!(parse_cidr("192.168.1.0").is_none());
        assert!(parse_cidr("invalid/24").is_none());
    }

    #[test]
    fn test_ip_in_cidr_match() {
        let network = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert!(ip_in_cidr(&ip, &network, 24));
    }

    #[test]
    fn test_ip_in_cidr_no_match() {
        let network = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 100));
        assert!(!ip_in_cidr(&ip, &network, 24));
    }

    #[test]
    fn test_ip_in_cidr_exact_match() {
        let network = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        assert!(ip_in_cidr(&ip, &network, 32));
    }

    #[test]
    fn test_ip_in_cidr_wide_range() {
        let network = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255));
        assert!(ip_in_cidr(&ip, &network, 8));
    }

    #[test]
    fn test_ip_in_cidr_zero_prefix() {
        let network = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(123, 45, 67, 89));
        assert!(ip_in_cidr(&ip, &network, 0));
    }

    #[test]
    fn test_ip_in_cidr_v4_v6_mismatch() {
        let network = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0));
        let ip = IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        assert!(!ip_in_cidr(&ip, &network, 24));
    }
}
