// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory sliding-window rate limiter for key-based analytics ingest,
//! keyed by `analytics_ingest_keys.id`.
//!
//! A port of `temps-error-tracking`'s `IngestRateLimiter`, re-keyed from
//! `project_id` to `key_id` (ADR-040 §4). Cardinality is bounded by the number
//! of active ingest keys — not by visitor or IP, which are unbounded — so a
//! plain `Mutex<HashMap>` is safe here even though the ingest surface it
//! protects is public by design.
//!
//! Known limitation, recorded rather than hidden: because the bucket is the
//! key, one abusive client burns the whole key's budget for every legitimate
//! visitor sharing it. A second per-`(key_id, ip)` sub-limit is the natural
//! follow-up; it is deliberately not implemented here because `Origin`/IP
//! trust in a cross-origin deployment needs its own decision.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const WINDOW: Duration = Duration::from_secs(60);

/// Reserved bucket id for requests whose presented key fails to resolve.
/// Real `analytics_ingest_keys.id` values are a Postgres `SERIAL` starting at
/// 1, so 0 can never collide with one.
const UNRESOLVED_KEY_BUCKET: i32 = 0;

/// Cap on unresolved-key attempts per minute, applied as a single global
/// bucket rather than per-IP: it bounds the DB-query cost a bot can inflict
/// by cycling through valid-shaped garbage keys (`resolve()`'s cache only
/// helps on an exact repeated string) without requiring the IP/Origin trust
/// decision this module's doc comment defers. Deliberately coarse — once
/// tripped, every unresolved-key attempt is rejected without a DB round trip
/// until the window clears, so a flood from one bad actor can delay another
/// client's simultaneous key typo. That trade favors protecting the database
/// over a diagnostic nicety.
pub const UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE: i32 = 300;

/// Requests per minute applied when a key row carries no explicit limit.
/// Matches the `rate_limit_per_minute` column default in
/// `m20260831_000001_create_analytics_ingest_keys`.
pub const DEFAULT_RATE_LIMIT_PER_MINUTE: i32 = 600;

#[derive(Debug, Clone, Default)]
pub struct AnalyticsIngestRateLimiter {
    entries: Arc<Mutex<HashMap<i32, Vec<Instant>>>>,
}

impl AnalyticsIngestRateLimiter {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns `true` if a request against `key_id` is allowed under
    /// `limit_per_minute`.
    ///
    /// `None` or a non-positive limit is **unlimited** (fail open), matching
    /// the column's documented semantics: an operator who clears the limit is
    /// asking for no limit, and a key that predates limiting must not start
    /// dropping data because the column happened to be `NULL`.
    pub async fn check(&self, key_id: i32, limit_per_minute: Option<i32>) -> bool {
        let limit = match limit_per_minute {
            Some(limit) if limit > 0 => limit as usize,
            _ => return true,
        };

        let now = Instant::now();
        let window_start = now - WINDOW;

        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(key_id).or_default();
        timestamps.retain(|t| *t > window_start);

        if timestamps.len() >= limit {
            return false;
        }

        timestamps.push(now);
        true
    }

    /// Drop a key's window, so a revoked or rotated key stops occupying memory
    /// and a re-minted key does not inherit a stale budget.
    pub async fn forget(&self, key_id: i32) {
        self.entries.lock().await.remove(&key_id);
    }

    /// Read-only: true if the global unresolved-key bucket is already
    /// saturated. Checked *before* paying for a DB lookup, so a flood of
    /// distinct garbage keys stops costing queries once it trips.
    pub async fn unresolved_budget_exhausted(&self) -> bool {
        let now = Instant::now();
        let window_start = now - WINDOW;
        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(UNRESOLVED_KEY_BUCKET).or_default();
        timestamps.retain(|t| *t > window_start);
        timestamps.len() >= UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE as usize
    }

    /// Record one confirmed unresolved-key attempt against the global
    /// bucket. Call only after `resolve()` has actually returned `None` —
    /// recording on every request (valid or not) would let an unresolved-key
    /// flood burn a budget that legitimate keyed traffic shares.
    pub async fn record_unresolved_attempt(&self) {
        let now = Instant::now();
        let window_start = now - WINDOW;
        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(UNRESOLVED_KEY_BUCKET).or_default();
        timestamps.retain(|t| *t > window_start);
        timestamps.push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_requests_within_limit() {
        let limiter = AnalyticsIngestRateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check(1, Some(5)).await);
        }
    }

    #[tokio::test]
    async fn blocks_requests_over_limit() {
        let limiter = AnalyticsIngestRateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check(1, Some(3)).await);
        }
        assert!(!limiter.check(1, Some(3)).await);
    }

    #[tokio::test]
    async fn different_keys_are_independent() {
        let limiter = AnalyticsIngestRateLimiter::new();
        for _ in 0..2 {
            assert!(limiter.check(1, Some(2)).await);
        }
        assert!(!limiter.check(1, Some(2)).await);
        // Key 2 has its own budget, unaffected by key 1's usage.
        assert!(limiter.check(2, Some(2)).await);
    }

    #[tokio::test]
    async fn none_and_non_positive_limits_fail_open() {
        let limiter = AnalyticsIngestRateLimiter::new();
        for _ in 0..100 {
            assert!(limiter.check(1, None).await);
            assert!(limiter.check(1, Some(0)).await);
            assert!(limiter.check(1, Some(-1)).await);
        }
    }

    #[tokio::test]
    async fn default_limit_matches_the_column_default() {
        let limiter = AnalyticsIngestRateLimiter::new();
        for _ in 0..DEFAULT_RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check(9, Some(DEFAULT_RATE_LIMIT_PER_MINUTE)).await);
        }
        assert!(!limiter.check(9, Some(DEFAULT_RATE_LIMIT_PER_MINUTE)).await);
    }

    #[tokio::test]
    async fn forget_releases_a_keys_window() {
        let limiter = AnalyticsIngestRateLimiter::new();
        assert!(limiter.check(3, Some(1)).await);
        assert!(!limiter.check(3, Some(1)).await);

        limiter.forget(3).await;

        assert!(limiter.check(3, Some(1)).await);
    }

    #[tokio::test]
    async fn unresolved_budget_is_not_exhausted_before_any_attempts() {
        let limiter = AnalyticsIngestRateLimiter::new();
        assert!(!limiter.unresolved_budget_exhausted().await);
    }

    #[tokio::test]
    async fn unresolved_budget_trips_after_the_limit_and_stays_independent_of_keyed_traffic() {
        let limiter = AnalyticsIngestRateLimiter::new();

        for _ in 0..UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE {
            assert!(!limiter.unresolved_budget_exhausted().await);
            limiter.record_unresolved_attempt().await;
        }

        assert!(
            limiter.unresolved_budget_exhausted().await,
            "the bucket must trip once the limit's worth of attempts were recorded"
        );

        // A resolved key's own budget is a separate bucket entirely (id 0 is
        // reserved and never assigned to a real key), so it must be unaffected.
        assert!(limiter.check(1, Some(5)).await);
    }
}
