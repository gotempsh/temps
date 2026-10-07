// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-client-IP request rate limiting for routed project traffic (#1288).
//!
//! Two policies feed this limiter, both stored as configuration the console
//! already edits:
//!
//! - the instance policy (`AppSettings.rate_limiting`), applied when its
//!   `enabled` flag is set, counted per client IP across all project traffic;
//! - a project/environment policy (`DeploymentConfig.security.rate_limiting`),
//!   applied when that level's security switch is on. Its limits replace the
//!   instance limits for that environment; a limit it leaves unset inherits
//!   the instance value. Counted per (environment, client IP).
//!
//! Blacklist entries from either active policy reject with 403; whitelist
//! entries are exempt from counting. Entries are IPs or CIDR ranges.
//!
//! Hot path: one sharded `DashMap` lookup (the same structure the connection
//! limiter uses) and at most two compare-and-swap loops on packed atomics. No
//! locks held across awaits, no I/O, no allocation once a client's entry
//! exists. Counting uses fixed one-minute and one-hour windows, so a client
//! can burst up to twice a limit across a window boundary.
//!
//! Memory is bounded by [`MAX_TRACKED_CLIENTS`] entries (roughly 100 bytes
//! each, about 10 MB at the cap) plus [`OVERFLOW_BUCKETS`] shared counters
//! (256 KB). Entries whose windows have both expired are swept at most once a
//! minute. At the cap, a client with no entry yet is counted in a shared
//! overflow bucket picked by a randomly seeded hash of (scope, client IP), so
//! limits keep applying to it: an attacker cannot fill the table to stop
//! being counted, and one environment's traffic filling the table does not
//! exempt another's clients. The cost lands only at saturation: clients that
//! share a bucket share its budget, so they can be limited early (degrade
//! toward stricter, never toward unlimited). Overflow admissions are counted
//! in [`RateLimiter::overflow_admissions`]. A client that gets its own entry
//! once space frees up takes over its overflow bucket's live counts, so
//! moving out of overflow never resets a budget mid-window, even under
//! concurrent requests (see `RateLimiter::take_overflow`).

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{fence, AtomicU64, Ordering};
use std::sync::Arc;

/// Upper bound on (scope, client IP) pairs tracked by one proxy process.
pub const MAX_TRACKED_CLIENTS: usize = 100_000;

/// Shared counters for clients that arrive while the table is full.
pub const OVERFLOW_BUCKETS: usize = 16_384;

/// Seconds between sweeps of expired entries.
const SWEEP_INTERVAL_SECS: u64 = 60;

/// Scope key for the instance policy. Environment ids are positive.
const INSTANCE_SCOPE: i64 = -1;

/// A resolved policy for one request. Limits of `0` are not enforced.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitPolicy<'a> {
    pub per_minute: u32,
    pub per_hour: u32,
    pub whitelist: &'a [String],
    pub blacklist: &'a [String],
}

impl RateLimitPolicy<'_> {
    fn has_limits(&self) -> bool {
        self.per_minute > 0 || self.per_hour > 0
    }
}

/// Which window rejected a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitWindow {
    Minute,
    Hour,
}

impl RateLimitWindow {
    pub fn as_str(&self) -> &'static str {
        match self {
            RateLimitWindow::Minute => "minute",
            RateLimitWindow::Hour => "hour",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitDecision {
    Allow,
    /// The client IP is on an active blacklist.
    Blacklisted,
    /// Over the limit; retry once the rejecting window rolls over.
    Limited {
        window: RateLimitWindow,
        retry_after_secs: u64,
    },
}

/// Which policy produced the decision, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitScope {
    Instance,
    Environment(i32),
}

impl RateLimitScope {
    fn key(&self) -> i64 {
        match self {
            RateLimitScope::Instance => INSTANCE_SCOPE,
            RateLimitScope::Environment(id) => i64::from(*id),
        }
    }
}

/// Fixed-window counters for one (scope, client IP). Each cell packs the
/// window index in the high 32 bits and the count in the low 32 bits so a
/// window rollover and an increment are a single compare-and-swap.
#[derive(Default)]
struct ClientWindows {
    minute: AtomicU64,
    hour: AtomicU64,
}

impl ClientWindows {
    /// Add `bucket`'s live counts to these counters.
    fn absorb(&self, bucket: &ClientWindows) {
        absorb_cell(&self.minute, bucket.minute.load(Ordering::Relaxed));
        absorb_cell(&self.hour, bucket.hour.load(Ordering::Relaxed));
    }
}

/// Add the count packed in `from` to `cell`. A count from a window older than
/// the cell's is already expired and is dropped; a cell on an older window is
/// expired and is replaced.
fn absorb_cell(cell: &AtomicU64, from: u64) {
    let (from_window, from_count) = unpack(from);
    if from_count == 0 {
        return;
    }
    let mut current = cell.load(Ordering::Relaxed);
    loop {
        let (window, count) = unpack(current);
        let next = if window == from_window {
            pack(window, count.saturating_add(from_count))
        } else if window < from_window {
            pack(from_window, from_count)
        } else {
            return;
        };
        match cell.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn pack(window: u32, count: u32) -> u64 {
    (u64::from(window) << 32) | u64::from(count)
}

fn unpack(cell: u64) -> (u32, u32) {
    ((cell >> 32) as u32, cell as u32)
}

/// Take one slot in `cell` for `window` if fewer than `limit` were taken.
/// `limit == 0` never touches the cell.
fn try_take(cell: &AtomicU64, window: u32, limit: u32) -> bool {
    if limit == 0 {
        return true;
    }
    let mut current = cell.load(Ordering::Relaxed);
    loop {
        let (current_window, count) = unpack(current);
        let next = if current_window != window {
            pack(window, 1)
        } else if count >= limit {
            return false;
        } else {
            pack(window, count + 1)
        };
        match cell.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

/// Whether `ip` matches any IP or CIDR entry. Unparseable entries are
/// ignored. Parses entries in place: no allocation.
pub fn ip_listed(entries: &[String], ip: IpAddr) -> bool {
    entries.iter().any(|entry| {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        if entry.contains('/') {
            ipnetwork::IpNetwork::from_str(entry)
                .map(|network| network.contains(ip))
                .unwrap_or(false)
        } else {
            IpAddr::from_str(entry)
                .map(|listed| normalize(listed) == ip)
                .unwrap_or(false)
        }
    })
}

/// Map IPv4-mapped IPv6 addresses to IPv4 so list entries match either form.
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

pub struct RateLimiter {
    clients: DashMap<(i64, IpAddr), Arc<ClientWindows>>,
    capacity: usize,
    /// Fallback counters once `clients` is at `capacity`.
    overflow: Box<[ClientWindows]>,
    /// Per-process random seed so clients cannot aim at a chosen bucket.
    overflow_hasher: RandomState,
    last_sweep_secs: AtomicU64,
    overflow_admissions: AtomicU64,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::with_capacity(MAX_TRACKED_CLIENTS)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            clients: DashMap::new(),
            capacity,
            overflow: (0..OVERFLOW_BUCKETS)
                .map(|_| ClientWindows::default())
                .collect(),
            overflow_hasher: RandomState::new(),
            last_sweep_secs: AtomicU64::new(0),
            overflow_admissions: AtomicU64::new(0),
        }
    }

    /// Decide one request. `now_secs` is Unix time, passed in for tests.
    pub fn check(
        &self,
        scope: RateLimitScope,
        ip: IpAddr,
        policy: &RateLimitPolicy<'_>,
        now_secs: u64,
    ) -> RateLimitDecision {
        let ip = normalize(ip);
        if ip_listed(policy.blacklist, ip) {
            return RateLimitDecision::Blacklisted;
        }
        if !policy.has_limits() || ip_listed(policy.whitelist, ip) {
            return RateLimitDecision::Allow;
        }

        self.maybe_sweep(now_secs);

        let key = (scope.key(), ip);
        match self.clients.get(&key) {
            Some(existing) => {
                let windows = Arc::clone(existing.value());
                drop(existing);
                Self::take(&windows, policy, now_secs)
            }
            None if self.clients.len() >= self.capacity => {
                self.take_overflow(key, policy, now_secs)
            }
            None => self.take_new(key, policy, now_secs),
        }
    }

    // A client can move between its overflow bucket and its own entry while
    // its windows are live: counted in overflow while the table is full, then
    // given an entry once a sweep frees space. Neither side may lose the
    // other's counts, including when both happen at once. Each side publishes
    // first (the bucket increment, the entry insert), then a SeqCst fence, then
    // reads the other; so at least one of them sees the other. The worst case
    // is a request counted twice: stricter, never a fresh budget.

    /// Count a request from an untracked client while the table is full.
    fn take_overflow(
        &self,
        key: (i64, IpAddr),
        policy: &RateLimitPolicy<'_>,
        now_secs: u64,
    ) -> RateLimitDecision {
        self.overflow_admissions.fetch_add(1, Ordering::Relaxed);
        let decision = Self::take(self.overflow_bucket(key), policy, now_secs);
        if decision != RateLimitDecision::Allow {
            return decision;
        }
        fence(Ordering::SeqCst);
        // The client got its own entry meanwhile: count there too.
        match self.clients.get(&key) {
            Some(existing) => {
                let windows = Arc::clone(existing.value());
                drop(existing);
                Self::take(&windows, policy, now_secs)
            }
            None => decision,
        }
    }

    /// Count a request from an untracked client while the table has room.
    fn take_new(
        &self,
        key: (i64, IpAddr),
        policy: &RateLimitPolicy<'_>,
        now_secs: u64,
    ) -> RateLimitDecision {
        let (windows, inserted) = match self.clients.entry(key) {
            Entry::Occupied(existing) => (Arc::clone(existing.get()), false),
            Entry::Vacant(vacant) => (Arc::clone(vacant.insert(Arc::default()).value()), true),
        };
        if inserted {
            fence(Ordering::SeqCst);
            // Carry over what the client used in its overflow bucket.
            windows.absorb(self.overflow_bucket(key));
        }
        Self::take(&windows, policy, now_secs)
    }

    fn overflow_bucket(&self, key: (i64, IpAddr)) -> &ClientWindows {
        &self.overflow[self.overflow_hasher.hash_one(key) as usize % self.overflow.len()]
    }

    fn take(
        windows: &ClientWindows,
        policy: &RateLimitPolicy<'_>,
        now_secs: u64,
    ) -> RateLimitDecision {
        let minute_window = (now_secs / 60) as u32;
        let hour_window = (now_secs / 3600) as u32;
        if !try_take(&windows.minute, minute_window, policy.per_minute) {
            return RateLimitDecision::Limited {
                window: RateLimitWindow::Minute,
                retry_after_secs: 60 - now_secs % 60,
            };
        }
        // A request the hourly limit rejects has already used a minute slot;
        // harmless, since the hourly limit is the binding one at that point.
        if !try_take(&windows.hour, hour_window, policy.per_hour) {
            return RateLimitDecision::Limited {
                window: RateLimitWindow::Hour,
                retry_after_secs: 3600 - now_secs % 3600,
            };
        }
        RateLimitDecision::Allow
    }

    /// Requests counted in a shared overflow bucket because the table was
    /// full when their client was first seen.
    pub fn overflow_admissions(&self) -> u64 {
        self.overflow_admissions.load(Ordering::Relaxed)
    }

    /// Number of (scope, client IP) entries currently tracked.
    pub fn tracked_clients(&self) -> usize {
        self.clients.len()
    }

    /// Drop entries whose minute and hour windows have both expired. Runs at
    /// most once per [`SWEEP_INTERVAL_SECS`], on the request that wins the
    /// compare-and-swap; every other request skips it.
    fn maybe_sweep(&self, now_secs: u64) {
        let last = self.last_sweep_secs.load(Ordering::Relaxed);
        if now_secs < last.saturating_add(SWEEP_INTERVAL_SECS) {
            return;
        }
        if self
            .last_sweep_secs
            .compare_exchange(last, now_secs, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let minute_window = (now_secs / 60) as u32;
        let hour_window = (now_secs / 3600) as u32;
        self.clients.retain(|_, windows| {
            let (minute, _) = unpack(windows.minute.load(Ordering::Relaxed));
            let (hour, _) = unpack(windows.hour.load(Ordering::Relaxed));
            minute == minute_window || hour == hour_window
        });
    }
}

/// Resolve the policy that applies to one routed request.
///
/// `instance` is `AppSettings.rate_limiting`; `project` is the effective
/// (project merged with environment) security config. Returns the scope to
/// count under, the policy, and the instance blacklist, which applies even
/// when a project policy replaces the instance limits. `None` when neither
/// policy is active.
pub fn resolve_policy<'a>(
    instance: &'a temps_core::RateLimitSettings,
    project: Option<&'a temps_entities::deployment_config::SecurityConfig>,
    environment_id: i32,
) -> Option<ResolvedRateLimit<'a>> {
    let project_rl = project
        .filter(|security| security.enabled == Some(true))
        .and_then(|security| security.rate_limiting.as_ref());
    let instance_blacklist: &'a [String] = if instance.enabled {
        &instance.blacklist_ips
    } else {
        &[]
    };

    if let Some(project_rl) = project_rl {
        return Some(ResolvedRateLimit {
            scope: RateLimitScope::Environment(environment_id),
            policy: RateLimitPolicy {
                per_minute: project_rl
                    .max_requests_per_minute
                    .unwrap_or(instance.max_requests_per_minute),
                per_hour: project_rl
                    .max_requests_per_hour
                    .unwrap_or(instance.max_requests_per_hour),
                whitelist: &project_rl.whitelist_ips,
                blacklist: &project_rl.blacklist_ips,
            },
            instance_blacklist,
        });
    }

    if instance.enabled {
        return Some(ResolvedRateLimit {
            scope: RateLimitScope::Instance,
            policy: RateLimitPolicy {
                per_minute: instance.max_requests_per_minute,
                per_hour: instance.max_requests_per_hour,
                whitelist: &instance.whitelist_ips,
                blacklist: &instance.blacklist_ips,
            },
            instance_blacklist: &[],
        });
    }
    None
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedRateLimit<'a> {
    pub scope: RateLimitScope,
    pub policy: RateLimitPolicy<'a>,
    /// Instance blacklist still in force under a project policy.
    pub instance_blacklist: &'a [String],
}

impl ResolvedRateLimit<'_> {
    /// Apply the resolved policy, including the instance blacklist.
    pub fn check(&self, limiter: &RateLimiter, ip: IpAddr, now_secs: u64) -> RateLimitDecision {
        if ip_listed(self.instance_blacklist, normalize(ip)) {
            return RateLimitDecision::Blacklisted;
        }
        limiter.check(self.scope, ip, &self.policy, now_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temps_core::RateLimitSettings;
    use temps_entities::deployment_config::{RateLimitConfig, SecurityConfig};

    const T0: u64 = 1_800_000_000; // minute- and hour-aligned
    const _: () = assert!(T0.is_multiple_of(3600));

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn policy(per_minute: u32, per_hour: u32) -> RateLimitPolicy<'static> {
        RateLimitPolicy {
            per_minute,
            per_hour,
            whitelist: &[],
            blacklist: &[],
        }
    }

    #[test]
    fn allows_up_to_the_minute_limit_then_rejects_with_retry_after() {
        let limiter = RateLimiter::new();
        let p = policy(2, 0);
        let scope = RateLimitScope::Environment(7);
        assert_eq!(
            limiter.check(scope, ip("203.0.113.5"), &p, T0),
            RateLimitDecision::Allow
        );
        assert_eq!(
            limiter.check(scope, ip("203.0.113.5"), &p, T0 + 1),
            RateLimitDecision::Allow
        );
        assert_eq!(
            limiter.check(scope, ip("203.0.113.5"), &p, T0 + 15),
            RateLimitDecision::Limited {
                window: RateLimitWindow::Minute,
                retry_after_secs: 45
            }
        );
        // A different client and a different environment have their own budget.
        assert_eq!(
            limiter.check(scope, ip("203.0.113.6"), &p, T0 + 15),
            RateLimitDecision::Allow
        );
        assert_eq!(
            limiter.check(
                RateLimitScope::Environment(8),
                ip("203.0.113.5"),
                &p,
                T0 + 15
            ),
            RateLimitDecision::Allow
        );
        // The next minute resets the window.
        assert_eq!(
            limiter.check(scope, ip("203.0.113.5"), &p, T0 + 60),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn hourly_limit_applies_across_minutes() {
        let limiter = RateLimiter::new();
        let p = policy(2, 5);
        let scope = RateLimitScope::Instance;
        let client = ip("198.51.100.1");
        let mut allowed = 0;
        for minute in 0..5 {
            for _ in 0..2 {
                if limiter.check(scope, client, &p, T0 + minute * 60) == RateLimitDecision::Allow {
                    allowed += 1;
                }
            }
        }
        assert_eq!(allowed, 5);
        assert_eq!(
            limiter.check(scope, client, &p, T0 + 600),
            RateLimitDecision::Limited {
                window: RateLimitWindow::Hour,
                retry_after_secs: 3000
            }
        );
        assert_eq!(
            limiter.check(scope, client, &p, T0 + 3600),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn blacklist_rejects_and_whitelist_skips_counting() {
        let limiter = RateLimiter::new();
        let whitelist = vec!["10.0.0.0/8".to_string()];
        let blacklist = vec![" 127.0.0.1 ".to_string(), "2001:db8::/32".to_string()];
        let p = RateLimitPolicy {
            per_minute: 1,
            per_hour: 0,
            whitelist: &whitelist,
            blacklist: &blacklist,
        };
        let scope = RateLimitScope::Environment(1);
        assert_eq!(
            limiter.check(scope, ip("127.0.0.1"), &p, T0),
            RateLimitDecision::Blacklisted
        );
        assert_eq!(
            limiter.check(scope, ip("::ffff:127.0.0.1"), &p, T0),
            RateLimitDecision::Blacklisted
        );
        assert_eq!(
            limiter.check(scope, ip("2001:db8::9"), &p, T0),
            RateLimitDecision::Blacklisted
        );
        for _ in 0..10 {
            assert_eq!(
                limiter.check(scope, ip("10.1.2.3"), &p, T0),
                RateLimitDecision::Allow
            );
        }
        assert_eq!(limiter.tracked_clients(), 0);
    }

    #[test]
    fn blacklist_applies_even_without_limits() {
        let limiter = RateLimiter::new();
        let blacklist = vec!["192.0.2.1".to_string()];
        let p = RateLimitPolicy {
            per_minute: 0,
            per_hour: 0,
            whitelist: &[],
            blacklist: &blacklist,
        };
        let scope = RateLimitScope::Instance;
        assert_eq!(
            limiter.check(scope, ip("192.0.2.1"), &p, T0),
            RateLimitDecision::Blacklisted
        );
        assert_eq!(
            limiter.check(scope, ip("192.0.2.2"), &p, T0),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn invalid_list_entries_are_ignored() {
        let entries = vec![
            "not-an-ip".to_string(),
            "10.0.0.0/99".to_string(),
            String::new(),
        ];
        assert!(!ip_listed(&entries, ip("10.0.0.1")));
    }

    #[test]
    fn sweep_drops_expired_entries_only() {
        let limiter = RateLimiter::new();
        let p = policy(10, 0);
        limiter.check(RateLimitScope::Instance, ip("192.0.2.10"), &p, T0);
        assert_eq!(limiter.tracked_clients(), 1);
        // Two hours later both windows have expired; the next request sweeps.
        limiter.check(RateLimitScope::Instance, ip("192.0.2.11"), &p, T0 + 7200);
        assert_eq!(limiter.tracked_clients(), 1);
    }

    #[test]
    fn clients_seen_at_capacity_are_still_limited() {
        let limiter = RateLimiter::with_capacity(2);
        let p = policy(3, 0);
        let env_a = RateLimitScope::Environment(1);
        limiter.check(env_a, ip("192.0.2.1"), &p, T0);
        limiter.check(env_a, ip("192.0.2.2"), &p, T0);
        assert_eq!(limiter.tracked_clients(), 2);

        // The table is full: a new client of the same environment, and one of
        // another environment, are counted in overflow buckets, not let through.
        for (scope, client) in [
            (env_a, ip("198.51.100.7")),
            (RateLimitScope::Environment(2), ip("203.0.113.9")),
        ] {
            for _ in 0..3 {
                assert_eq!(
                    limiter.check(scope, client, &p, T0),
                    RateLimitDecision::Allow
                );
            }
            assert!(matches!(
                limiter.check(scope, client, &p, T0),
                RateLimitDecision::Limited {
                    window: RateLimitWindow::Minute,
                    ..
                }
            ));
        }
        assert_eq!(limiter.tracked_clients(), 2);
        assert!(limiter.overflow_admissions() >= 8);

        // The overflow window rolls over like any other.
        assert_eq!(
            limiter.check(env_a, ip("198.51.100.7"), &p, T0 + 60),
            RateLimitDecision::Allow
        );
    }

    #[test]
    fn leaving_overflow_keeps_the_hourly_budget_used() {
        let limiter = RateLimiter::with_capacity(1);
        let p = policy(0, 5);
        let scope = RateLimitScope::Instance;
        let client = ip("198.51.100.7");
        // A minute-only entry fills the table; the client is counted in overflow.
        limiter.check(scope, ip("192.0.2.1"), &policy(1, 0), T0);
        for _ in 0..3 {
            assert_eq!(
                limiter.check(scope, client, &p, T0),
                RateLimitDecision::Allow
            );
        }
        assert_eq!(limiter.tracked_clients(), 1);

        // Two minutes later the sweep frees the slot and the client gets its
        // own entry, still inside the same hour: only 2 of 5 remain.
        let later = T0 + 120;
        for _ in 0..2 {
            assert_eq!(
                limiter.check(scope, client, &p, later),
                RateLimitDecision::Allow
            );
        }
        assert_eq!(limiter.tracked_clients(), 1, "the client is tracked now");
        assert!(matches!(
            limiter.check(scope, client, &p, later),
            RateLimitDecision::Limited {
                window: RateLimitWindow::Hour,
                ..
            }
        ));
    }

    #[test]
    fn overflow_requests_also_count_on_an_entry_created_meanwhile() {
        let limiter = RateLimiter::with_capacity(1);
        let p = policy(0, 5);
        let key = (INSTANCE_SCOPE, ip("198.51.100.7"));
        // Another request created the client's entry between this request's
        // capacity check and its overflow increment.
        limiter.clients.insert(key, Arc::default());
        for _ in 0..5 {
            assert_eq!(limiter.take_overflow(key, &p, T0), RateLimitDecision::Allow);
        }
        let entry = Arc::clone(limiter.clients.get(&key).expect("entry").value());
        assert_eq!(unpack(entry.hour.load(Ordering::Relaxed)).1, 5);
        assert!(matches!(
            limiter.check(RateLimitScope::Instance, key.1, &p, T0),
            RateLimitDecision::Limited {
                window: RateLimitWindow::Hour,
                ..
            }
        ));
    }

    #[test]
    fn absorb_adds_live_counts_and_drops_expired_ones() {
        let cell = AtomicU64::new(pack(10, 2));
        absorb_cell(&cell, pack(10, 3));
        assert_eq!(unpack(cell.load(Ordering::Relaxed)), (10, 5));
        absorb_cell(&cell, pack(9, 7));
        assert_eq!(
            unpack(cell.load(Ordering::Relaxed)),
            (10, 5),
            "older window"
        );
        absorb_cell(&cell, pack(11, 1));
        assert_eq!(
            unpack(cell.load(Ordering::Relaxed)),
            (11, 1),
            "cell was expired"
        );
        absorb_cell(&cell, pack(11, 0));
        assert_eq!(unpack(cell.load(Ordering::Relaxed)), (11, 1));
    }

    #[test]
    fn concurrent_requests_never_exceed_the_limit() {
        let limiter = Arc::new(RateLimiter::new());
        let allowed = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                let allowed = Arc::clone(&allowed);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        let decision = limiter.check(
                            RateLimitScope::Environment(3),
                            "198.51.100.77".parse().unwrap(),
                            &policy(50, 0),
                            T0,
                        );
                        if decision == RateLimitDecision::Allow {
                            allowed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(allowed.load(Ordering::Relaxed), 50);
    }

    fn instance(enabled: bool) -> RateLimitSettings {
        RateLimitSettings {
            enabled,
            max_requests_per_minute: 60,
            max_requests_per_hour: 1000,
            whitelist_ips: vec!["10.0.0.1".into()],
            blacklist_ips: vec!["192.0.2.66".into()],
        }
    }

    fn project(enabled: Option<bool>, rl: Option<RateLimitConfig>) -> SecurityConfig {
        SecurityConfig {
            enabled,
            rate_limiting: rl,
            ..Default::default()
        }
    }

    #[test]
    fn nothing_applies_when_both_policies_are_off() {
        let settings = instance(false);
        let security = project(None, None);
        assert!(resolve_policy(&settings, Some(&security), 4).is_none());
        assert!(resolve_policy(&settings, None, 4).is_none());
    }

    #[test]
    fn instance_policy_counts_per_ip_across_projects() {
        let settings = instance(true);
        let resolved = resolve_policy(&settings, None, 4).unwrap();
        assert_eq!(resolved.scope, RateLimitScope::Instance);
        assert_eq!(resolved.policy.per_minute, 60);
        assert_eq!(resolved.policy.per_hour, 1000);
    }

    #[test]
    fn project_policy_overrides_limits_and_inherits_unset_values() {
        let settings = instance(false);
        let security = project(
            Some(true),
            Some(RateLimitConfig {
                max_requests_per_minute: Some(2),
                max_requests_per_hour: None,
                whitelist_ips: vec![],
                blacklist_ips: vec!["127.0.0.1".into()],
            }),
        );
        let resolved = resolve_policy(&settings, Some(&security), 4).unwrap();
        assert_eq!(resolved.scope, RateLimitScope::Environment(4));
        assert_eq!(resolved.policy.per_minute, 2);
        assert_eq!(resolved.policy.per_hour, 1000);
        // Instance policy is off, so its blacklist does not apply.
        assert!(resolved.instance_blacklist.is_empty());

        let limiter = RateLimiter::new();
        assert_eq!(
            resolved.check(&limiter, ip("127.0.0.1"), T0),
            RateLimitDecision::Blacklisted
        );
    }

    #[test]
    fn project_switch_off_leaves_project_lists_unenforced() {
        let settings = instance(false);
        let security = project(
            Some(false),
            Some(RateLimitConfig {
                max_requests_per_minute: Some(1),
                max_requests_per_hour: None,
                whitelist_ips: vec![],
                blacklist_ips: vec!["127.0.0.1".into()],
            }),
        );
        assert!(resolve_policy(&settings, Some(&security), 4).is_none());
    }

    #[test]
    fn instance_blacklist_still_applies_under_a_project_policy() {
        let settings = instance(true);
        let security = project(
            Some(true),
            Some(RateLimitConfig {
                max_requests_per_minute: Some(500),
                max_requests_per_hour: None,
                whitelist_ips: vec![],
                blacklist_ips: vec![],
            }),
        );
        let resolved = resolve_policy(&settings, Some(&security), 4).unwrap();
        let limiter = RateLimiter::new();
        assert_eq!(
            resolved.check(&limiter, ip("192.0.2.66"), T0),
            RateLimitDecision::Blacklisted
        );
        assert_eq!(
            resolved.check(&limiter, ip("192.0.2.67"), T0),
            RateLimitDecision::Allow
        );
    }
}
