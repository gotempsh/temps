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

use arc_swap::ArcSwap;
use async_trait::async_trait;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use temps_database::DbConnection;
use tracing::{debug, error, warn};

use pingora_core::listeners::ConnectionFilter;

/// How often to refresh the blocked IP cache from the database
const CACHE_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long to wait before retrying after a failed refresh. Shorter than the
/// normal interval so a blocklist change made during a database blip is
/// picked up soon after it recovers, but a fixed delay rather than "next
/// connection" so an outage costs one query per interval, not one per
/// accepted connection.
const FAILED_REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Upper bound on a single refresh query. A refresh that hangs would
/// otherwise hold the single-flight slot forever and freeze the blocklist.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the blocklist comes from: the `ip_access_control` table in
/// production, a stub in tests.
#[async_trait]
trait BlocklistSource: Send + Sync + 'static {
    /// Every blocked entry as text: a single address or a CIDR.
    async fn load_blocked(&self) -> Result<Vec<String>, sea_orm::DbErr>;
}

struct DbBlocklistSource {
    db: Arc<DbConnection>,
}

#[async_trait]
impl BlocklistSource for DbBlocklistSource {
    async fn load_blocked(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        use sea_orm::{ConnectionTrait, Statement};

        let sql = "SELECT ip_address::text FROM ip_access_control WHERE action = 'block'";
        let rows = self
            .db
            .query_all(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                sql.to_string(),
            ))
            .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            match row.try_get::<String>("", "ip_address") {
                Ok(ip) => entries.push(ip),
                Err(e) => warn!("Failed to parse IP from database row: {}", e),
            }
        }
        Ok(entries)
    }
}

/// An immutable blocklist snapshot, swapped in whole on every refresh.
#[derive(Debug, Default)]
struct Blocklist {
    /// Blocked individual IPs (exact matches)
    ips: HashSet<IpAddr>,
    /// Blocked CIDR ranges as (network, prefix_len)
    cidrs: Vec<(IpAddr, u8)>,
}

impl Blocklist {
    fn from_entries(entries: Vec<String>) -> Self {
        let mut blocklist = Self::default();
        for entry in entries {
            if entry.contains('/') {
                // CIDR notation
                match parse_cidr(&entry) {
                    Some(cidr) => blocklist.cidrs.push(cidr),
                    None => warn!("Invalid CIDR in blocklist: {}", entry),
                }
            } else {
                match entry.parse::<IpAddr>() {
                    Ok(ip) => {
                        blocklist.ips.insert(ip);
                    }
                    Err(_) => warn!("Invalid IP address in blocklist: {}", entry),
                }
            }
        }
        blocklist
    }

    /// Exact match (O(1)), then CIDR containment.
    fn contains(&self, ip: &IpAddr) -> bool {
        self.ips.contains(ip)
            || self
                .cidrs
                .iter()
                .any(|(network, prefix_len)| ip_in_cidr(ip, network, *prefix_len))
    }
}

/// Refresh timing, overridable so tests do not wait for real intervals.
#[derive(Debug, Clone, Copy)]
struct RefreshPolicy {
    interval: Duration,
    retry_after_failure: Duration,
    timeout: Duration,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            interval: CACHE_REFRESH_INTERVAL,
            retry_after_failure: FAILED_REFRESH_RETRY_INTERVAL,
            timeout: REFRESH_TIMEOUT,
        }
    }
}

struct FilterState {
    blocklist: ArcSwap<Blocklist>,
    /// Single-flight guard: at most one refresh task exists at a time.
    refresh_in_flight: AtomicBool,
    /// Milliseconds since `epoch` before which no refresh may start. Set
    /// after every attempt — success or failure — so a failing database is
    /// retried on a timer, never per connection.
    next_refresh_ms: AtomicU64,
    epoch: Instant,
    policy: RefreshPolicy,
    source: Arc<dyn BlocklistSource>,
}

impl FilterState {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn schedule_next_refresh(&self, after: Duration) {
        let after_ms = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        self.next_refresh_ms
            .store(self.now_ms().saturating_add(after_ms), Ordering::Release);
    }

    fn refresh_due(&self) -> bool {
        self.now_ms() >= self.next_refresh_ms.load(Ordering::Acquire)
    }

    async fn refresh(&self) {
        match tokio::time::timeout(self.policy.timeout, self.source.load_blocked()).await {
            Ok(Ok(entries)) => {
                let blocklist = Blocklist::from_entries(entries);
                debug!(
                    "Refreshed TCP connection filter cache: {} IPs, {} CIDRs",
                    blocklist.ips.len(),
                    blocklist.cidrs.len()
                );
                self.blocklist.store(Arc::new(blocklist));
                self.schedule_next_refresh(self.policy.interval);
            }
            Ok(Err(e)) => {
                error!(
                    "Failed to refresh TCP connection filter cache: {} — keeping the previous blocklist, retrying in {:?}",
                    e, self.policy.retry_after_failure
                );
                self.schedule_next_refresh(self.policy.retry_after_failure);
            }
            Err(_) => {
                error!(
                    "TCP connection filter cache refresh timed out after {:?} — keeping the previous blocklist, retrying in {:?}",
                    self.policy.timeout, self.policy.retry_after_failure
                );
                self.schedule_next_refresh(self.policy.retry_after_failure);
            }
        }
    }
}

/// Releases the single-flight slot when the refresh task ends, however it
/// ends. A task that never reached `refresh`'s own scheduling (it panicked
/// or was dropped at runtime shutdown) is treated as a failure, so the
/// next attempt still waits for the retry interval.
struct InFlightGuard {
    state: Arc<FilterState>,
    completed: bool,
}

impl InFlightGuard {
    /// The refresh scheduled its own next attempt; don't override it.
    fn mark_completed(&mut self) {
        self.completed = true;
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.state
                .schedule_next_refresh(self.state.policy.retry_after_failure);
        }
        self.state.refresh_in_flight.store(false, Ordering::Release);
    }
}

/// Cached IP blocklist for TCP-level connection filtering.
///
/// The per-connection path takes no lock and does no I/O: one atomic
/// timestamp check, one `ArcSwap` load and a set lookup. The blocklist is
/// refreshed from the database in the background by at most one task at a
/// time.
pub struct TcpConnectionFilter {
    state: Arc<FilterState>,
}

impl std::fmt::Debug for TcpConnectionFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let blocklist = self.state.blocklist.load();
        f.debug_struct("TcpConnectionFilter")
            .field("blocked_ips", &blocklist.ips.len())
            .field("blocked_cidrs", &blocklist.cidrs.len())
            .field(
                "refresh_in_flight",
                &self.state.refresh_in_flight.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl TcpConnectionFilter {
    /// Create a new TcpConnectionFilter.
    ///
    /// The cache starts empty and is lazily populated on the first `should_accept` call.
    /// This avoids calling `tokio::spawn` from the constructor, which would panic in
    /// Pingora's synchronous startup context (no Tokio runtime available).
    pub fn new(db: Arc<DbConnection>) -> Self {
        Self::with_source(Arc::new(DbBlocklistSource { db }), RefreshPolicy::default())
    }

    fn with_source(source: Arc<dyn BlocklistSource>, policy: RefreshPolicy) -> Self {
        Self {
            state: Arc::new(FilterState {
                blocklist: ArcSwap::from_pointee(Blocklist::default()),
                refresh_in_flight: AtomicBool::new(false),
                // Due immediately: the first connection triggers a refresh.
                next_refresh_ms: AtomicU64::new(0),
                epoch: Instant::now(),
                policy,
                source,
            }),
        }
    }

    /// Check if an IP address is blocked (exact match or CIDR containment)
    fn is_ip_blocked(&self, ip: &IpAddr) -> bool {
        self.state.blocklist.load().contains(ip)
    }

    /// Start a background refresh if one is due and none is running.
    fn maybe_refresh(&self) {
        let state = &self.state;
        if !state.refresh_due() {
            return;
        }
        if state
            .refresh_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Another task may have finished a refresh between the due check
        // and taking the slot; don't start a second one back to back.
        if !state.refresh_due() {
            state.refresh_in_flight.store(false, Ordering::Release);
            return;
        }

        let mut guard = InFlightGuard {
            state: Arc::clone(state),
            completed: false,
        };
        tokio::spawn(async move {
            guard.state.refresh().await;
            guard.mark_completed();
        });
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

    // ── Refresh single-flight and backoff ──

    /// Counts loads; each load waits for `gate` permits when gated, then
    /// returns `entries` or a database error.
    struct StubSource {
        loads: AtomicU64,
        gate: Option<tokio::sync::Semaphore>,
        result: std::sync::Mutex<Result<Vec<String>, String>>,
    }

    impl StubSource {
        fn ok(entries: &[&str]) -> Self {
            Self {
                loads: AtomicU64::new(0),
                gate: None,
                result: std::sync::Mutex::new(Ok(entries.iter().map(|e| e.to_string()).collect())),
            }
        }

        fn failing() -> Self {
            Self {
                loads: AtomicU64::new(0),
                gate: None,
                result: std::sync::Mutex::new(Err("connection refused".to_string())),
            }
        }

        fn gated(entries: &[&str]) -> Self {
            Self {
                gate: Some(tokio::sync::Semaphore::new(0)),
                ..Self::ok(entries)
            }
        }

        fn start_failing(&self) {
            *self.result.lock().unwrap() = Err("connection refused".to_string());
        }

        fn loads(&self) -> u64 {
            self.loads.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl BlocklistSource for StubSource {
        async fn load_blocked(&self) -> Result<Vec<String>, sea_orm::DbErr> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                let _permit = gate.acquire().await;
            }
            self.result
                .lock()
                .unwrap()
                .clone()
                .map_err(sea_orm::DbErr::Custom)
        }
    }

    fn test_policy() -> RefreshPolicy {
        RefreshPolicy {
            interval: Duration::from_secs(3600),
            retry_after_failure: Duration::from_secs(3600),
            timeout: Duration::from_secs(5),
        }
    }

    fn filter_with(source: &Arc<StubSource>, policy: RefreshPolicy) -> TcpConnectionFilter {
        TcpConnectionFilter::with_source(Arc::clone(source) as Arc<dyn BlocklistSource>, policy)
    }

    async fn wait_until_idle(filter: &TcpConnectionFilter) {
        for _ in 0..500 {
            if !filter.state.refresh_in_flight.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("refresh still in flight after 1s");
    }

    fn addr(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 443)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_connections_start_a_single_refresh() {
        let source = Arc::new(StubSource::gated(&["203.0.113.7"]));
        let filter = Arc::new(filter_with(&source, test_policy()));

        let accepts: Vec<_> = (0..200)
            .map(|_| {
                let filter = Arc::clone(&filter);
                tokio::spawn(async move { filter.should_accept(Some(&addr("198.51.100.1"))).await })
            })
            .collect();
        for accept in accepts {
            assert!(accept.await.unwrap());
        }
        // Let the spawned refresh reach the gate.
        for _ in 0..500 {
            if source.loads() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(source.loads(), 1, "one refresh for 200 stale connections");

        source.gate.as_ref().unwrap().add_permits(1);
        wait_until_idle(&filter).await;
        assert!(!filter.should_accept(Some(&addr("203.0.113.7"))).await);
        assert_eq!(source.loads(), 1, "fresh cache must not refresh again");
    }

    #[tokio::test]
    async fn failed_refresh_backs_off_instead_of_retrying_per_connection() {
        let source = Arc::new(StubSource::failing());
        let filter = filter_with(&source, test_policy());

        assert!(filter.should_accept(Some(&addr("198.51.100.1"))).await);
        wait_until_idle(&filter).await;
        assert_eq!(source.loads(), 1);

        for _ in 0..500 {
            assert!(filter.should_accept(Some(&addr("198.51.100.1"))).await);
        }
        wait_until_idle(&filter).await;
        assert_eq!(
            source.loads(),
            1,
            "an outage must not cost a query per connection"
        );
    }

    #[tokio::test]
    async fn failed_refresh_retries_after_the_backoff() {
        let source = Arc::new(StubSource::failing());
        let filter = filter_with(
            &source,
            RefreshPolicy {
                retry_after_failure: Duration::from_millis(20),
                ..test_policy()
            },
        );

        filter.should_accept(None).await;
        wait_until_idle(&filter).await;
        assert_eq!(source.loads(), 1);

        tokio::time::sleep(Duration::from_millis(40)).await;
        filter.should_accept(None).await;
        wait_until_idle(&filter).await;
        assert_eq!(source.loads(), 2);
    }

    #[tokio::test]
    async fn failed_refresh_keeps_the_previous_blocklist() {
        let source = Arc::new(StubSource::ok(&["10.0.0.0/8"]));
        let filter = filter_with(&source, test_policy());
        filter.should_accept(None).await;
        wait_until_idle(&filter).await;
        assert!(!filter.should_accept(Some(&addr("10.1.2.3"))).await);

        // The database goes away; force the next refresh to be due.
        source.start_failing();
        filter.state.next_refresh_ms.store(0, Ordering::SeqCst);
        filter.should_accept(None).await;
        wait_until_idle(&filter).await;

        assert_eq!(source.loads(), 2);
        assert!(!filter.should_accept(Some(&addr("10.1.2.3"))).await);
        assert!(filter.should_accept(Some(&addr("192.0.2.1"))).await);
    }

    #[tokio::test]
    async fn hung_refresh_times_out_and_frees_the_slot() {
        // Gated with no permits: the load never returns on its own.
        let source = Arc::new(StubSource::gated(&[]));
        let filter = filter_with(
            &source,
            RefreshPolicy {
                timeout: Duration::from_millis(20),
                retry_after_failure: Duration::from_millis(20),
                ..test_policy()
            },
        );

        filter.should_accept(None).await;
        wait_until_idle(&filter).await;
        assert_eq!(source.loads(), 1);

        // The timed-out attempt counts as a failure: a new refresh starts
        // after the backoff, not never.
        tokio::time::sleep(Duration::from_millis(40)).await;
        filter.should_accept(None).await;
        wait_until_idle(&filter).await;
        assert_eq!(source.loads(), 2);
    }

    #[test]
    fn blocklist_parses_ips_and_cidrs_and_skips_garbage() {
        let blocklist = Blocklist::from_entries(vec![
            "192.0.2.1".to_string(),
            "2001:db8::/32".to_string(),
            "not-an-ip".to_string(),
            "10.0.0.0/99".to_string(),
        ]);
        assert_eq!(blocklist.ips.len(), 1);
        assert_eq!(blocklist.cidrs.len(), 1);
        assert!(blocklist.contains(&"192.0.2.1".parse().unwrap()));
        assert!(blocklist.contains(&"2001:db8::1".parse().unwrap()));
        assert!(!blocklist.contains(&"192.0.2.2".parse().unwrap()));
    }
}
