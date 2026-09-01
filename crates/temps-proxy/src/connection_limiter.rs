// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-upstream concurrent-connection limiting for the proxy hot path.
//!
//! Protects the proxy's own connection/file-descriptor budget from a single
//! slow or malicious customer upstream. Independent of request/idle timeouts
//! in `RequestTimeoutSettings`, which bound how long a connection may stay
//! open — not how many may exist at once. See issue #646.

use dashmap::DashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Bounds how many distinct keys (project/environment ids) this node will
/// track counters for, so a pathological number of distinct short-lived
/// projects can't turn this into an unbounded memory sink. Far above any
/// realistic single-node project count; existing entries are only evicted
/// when at capacity and idle (count == 0).
const MAX_TRACKED_ENTRIES: usize = 50_000;

/// Tracks in-flight request counts per key (project or environment id) and
/// enforces a configurable concurrency ceiling per key. Hot path: lock-free
/// atomics only, no I/O, no request-path allocation beyond the DashMap
/// entry API. See issue #646.
#[derive(Default)]
pub struct ConnectionLimiter {
    counts: DashMap<i32, Arc<AtomicI64>>,
}

impl ConnectionLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attempts to reserve an in-flight slot for `key` under `limit`.
    /// `limit == 0` means unlimited: always succeeds and does no tracking
    /// at all (no entry created, no atomic touched) so unlimited keys never
    /// pay any cost. Returns `None` when `key` is already at `limit` — the
    /// caller should reject the request outright (e.g. 503) rather than
    /// queue it, so a stalled tenant can't also exhaust the proxy's own
    /// queuing memory.
    pub fn try_acquire(&self, key: i32, limit: u32) -> Option<ConnectionPermit> {
        if limit == 0 {
            return Some(ConnectionPermit { counter: None });
        }
        // Opportunistic bounded eviction: only when we're about to grow the
        // map past the cap and `key` isn't already tracked. Mirrors
        // PreviewAuthLimiter's eviction idiom.
        if !self.counts.contains_key(&key) && self.counts.len() >= MAX_TRACKED_ENTRIES {
            if let Some(victim) = self
                .counts
                .iter()
                .find(|e| e.value().load(Ordering::Relaxed) == 0)
                .map(|e| *e.key())
            {
                self.counts.remove(&victim);
            }
            // If nothing idle was found to evict, fall through: the map
            // grows slightly past the cap rather than incorrectly denying
            // service. This can only happen if >MAX_TRACKED_ENTRIES keys
            // are simultaneously mid-request, which is already far beyond
            // any single node's realistic capacity.
        }
        let counter = self
            .counts
            .entry(key)
            .or_insert_with(|| Arc::new(AtomicI64::new(0)))
            .clone();
        loop {
            let current = counter.load(Ordering::Relaxed);
            if current >= limit as i64 {
                return None;
            }
            if counter
                .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Some(ConnectionPermit {
                    counter: Some(counter),
                });
            }
        }
    }

    /// Current in-flight count for `key` (0 if untracked). For tests and
    /// potential future metrics surfacing.
    #[cfg(test)]
    pub fn current(&self, key: i32) -> i64 {
        self.counts
            .get(&key)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

/// RAII guard: releases the reserved in-flight slot when dropped, i.e. when
/// the request finishes (success, error, or early bail-out) — held by
/// `ProxyContext` for the request's lifetime and dropped in `logging()`,
/// which Pingora calls exactly once per request regardless of outcome.
pub struct ConnectionPermit {
    counter: Option<Arc<AtomicI64>>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        if let Some(counter) = &self.counter {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_always_succeeds_and_tracks_nothing() {
        let limiter = ConnectionLimiter::new();
        let _p1 = limiter.try_acquire(1, 0).expect("unlimited must succeed");
        let _p2 = limiter.try_acquire(1, 0).expect("unlimited must succeed");
        assert_eq!(limiter.current(1), 0, "unlimited key should not be tracked");
    }

    #[test]
    fn rejects_at_limit_and_frees_on_drop() {
        let limiter = ConnectionLimiter::new();
        let p1 = limiter.try_acquire(1, 2).expect("under limit");
        let p2 = limiter.try_acquire(1, 2).expect("at limit, still ok");
        assert_eq!(limiter.current(1), 2);
        assert!(
            limiter.try_acquire(1, 2).is_none(),
            "over limit must reject"
        );

        drop(p1);
        assert_eq!(limiter.current(1), 1);
        let _p3 = limiter.try_acquire(1, 2).expect("slot freed after drop");

        drop(p2);
    }

    #[test]
    fn different_keys_are_independent() {
        let limiter = ConnectionLimiter::new();
        let _p1 = limiter.try_acquire(1, 1).expect("key 1 under limit");
        let _p2 = limiter
            .try_acquire(2, 1)
            .expect("key 2 under limit, independent of key 1");
        assert!(limiter.try_acquire(1, 1).is_none());
        assert!(limiter.try_acquire(2, 1).is_none());
    }
}
