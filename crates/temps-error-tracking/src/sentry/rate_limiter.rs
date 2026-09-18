// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! In-memory sliding-window rate limiter for Sentry ingest, keyed by project id.
//!
//! Cardinality is bounded by the number of projects with error tracking
//! enabled — not by visitor/IP, which is unbounded — so a plain
//! `Mutex<HashMap>` is safe to hold here even though ingest is a public
//! surface (the tunnel route resolves the project from a DSN when one is
//! offered, and from `Host` otherwise). This is not the proxy hot path;
//! it runs at the same order of magnitude as `temps-auth`'s per-IP limiter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const WINDOW: Duration = Duration::from_secs(60);

/// Reserved bucket id for requests whose presented credential fails to
/// resolve. Real `projects.id` values are a Postgres `SERIAL` starting at 1,
/// so 0 can never collide with a project's own bucket.
const UNRESOLVED_CREDENTIAL_BUCKET: i32 = 0;

/// Cap on unresolved-credential attempts per minute, applied as a single
/// global bucket rather than per-IP.
///
/// The per-project buckets [`IngestRateLimiter::check`] maintains cannot cover
/// a request that never resolved to a project — there is no bucket to charge —
/// so before this existed, every forged `?sentry_key=` cost an uncached DB
/// query on an unauthenticated route reachable from any customer domain, with
/// no ceiling at all. `DSNService`'s resolution cache only helps on an exact
/// repeated string, so distinct garbage keys each missed it.
///
/// Global rather than per-IP on purpose: the per-IP variant needs a trust
/// decision about `X-Forwarded-For` that this route deliberately does not make
/// for admission control, and an unbounded per-IP map is itself a memory
/// amplification vector. The cost is bluntness — once tripped, a simultaneous
/// honest typo from another operator also gets a 429 until the window clears.
/// That trade favours protecting the database. Mirrors
/// `temps-analytics`'s `UNRESOLVED_KEY_RATE_LIMIT_PER_MINUTE` exactly.
pub const UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE: i32 = 300;

#[derive(Debug, Clone)]
pub struct IngestRateLimiter {
    entries: Arc<Mutex<HashMap<i32, Vec<Instant>>>>,
}

impl Default for IngestRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestRateLimiter {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns `true` if a request for `project_id` is allowed under
    /// `limit_per_minute`. `None` or a non-positive limit is treated as
    /// unlimited (fail open) — matches the semantics of the DSN column,
    /// where `NULL`/pre-existing rows have never been rate limited.
    pub async fn check(&self, project_id: i32, limit_per_minute: Option<i32>) -> bool {
        let limit = match limit_per_minute {
            Some(limit) if limit > 0 => limit as usize,
            _ => return true,
        };

        let now = Instant::now();
        let window_start = now - WINDOW;

        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(project_id).or_default();
        timestamps.retain(|t| *t > window_start);

        if timestamps.len() >= limit {
            return false;
        }

        timestamps.push(now);
        true
    }

    /// Read-only variant of [`Self::check`]: reports whether `project_id` has
    /// remaining budget without consuming a slot.
    ///
    /// Used to gate expensive work (decompression) on a *tentative* project
    /// attribution before the final one is known, without charging that
    /// project for a request it may turn out not to receive — see the tunnel
    /// handler's `Host`-then-embedded-DSN resolution, where the two can
    /// legitimately disagree. The actual charge always happens exactly once,
    /// against whichever project the request is finally attributed to, via
    /// [`Self::check`].
    pub async fn peek(&self, project_id: i32, limit_per_minute: Option<i32>) -> bool {
        let limit = match limit_per_minute {
            Some(limit) if limit > 0 => limit as usize,
            _ => return true,
        };

        let now = Instant::now();
        let window_start = now - WINDOW;

        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(project_id).or_default();
        timestamps.retain(|t| *t > window_start);

        timestamps.len() < limit
    }

    /// Read-only: `true` if the global unresolved-credential bucket is already
    /// saturated. Checked *before* paying for a DB lookup (and before any
    /// decompression), so a flood of distinct forged keys stops costing
    /// queries and CPU once it trips.
    pub async fn unresolved_budget_exhausted(&self) -> bool {
        let now = Instant::now();
        let window_start = now - WINDOW;
        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(UNRESOLVED_CREDENTIAL_BUCKET).or_default();
        timestamps.retain(|t| *t > window_start);
        timestamps.len() >= UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE as usize
    }

    /// Record one *confirmed* unresolved-credential attempt.
    ///
    /// Call only once a lookup has actually come back empty. Recording
    /// optimistically on every credential-bearing request would let a flood
    /// burn a budget that legitimate traffic shares, turning a DoS mitigation
    /// into the DoS.
    pub async fn record_unresolved_attempt(&self) {
        let now = Instant::now();
        let window_start = now - WINDOW;
        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(UNRESOLVED_CREDENTIAL_BUCKET).or_default();
        timestamps.retain(|t| *t > window_start);
        timestamps.push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_requests_within_limit() {
        let limiter = IngestRateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check(1, Some(5)).await);
        }
    }

    #[tokio::test]
    async fn blocks_requests_over_limit() {
        let limiter = IngestRateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check(1, Some(3)).await);
        }
        assert!(!limiter.check(1, Some(3)).await);
    }

    #[tokio::test]
    async fn different_projects_are_independent() {
        let limiter = IngestRateLimiter::new();
        for _ in 0..2 {
            assert!(limiter.check(1, Some(2)).await);
        }
        assert!(!limiter.check(1, Some(2)).await);
        // Project 2 has its own budget, unaffected by project 1's usage.
        assert!(limiter.check(2, Some(2)).await);
    }

    #[tokio::test]
    async fn unresolved_budget_is_not_exhausted_before_any_attempts() {
        let limiter = IngestRateLimiter::new();
        assert!(!limiter.unresolved_budget_exhausted().await);
    }

    #[tokio::test]
    async fn unresolved_budget_trips_after_the_limit_and_leaves_projects_alone() {
        let limiter = IngestRateLimiter::new();

        for _ in 0..UNRESOLVED_CREDENTIAL_LIMIT_PER_MINUTE {
            assert!(!limiter.unresolved_budget_exhausted().await);
            limiter.record_unresolved_attempt().await;
        }

        assert!(
            limiter.unresolved_budget_exhausted().await,
            "the bucket must trip once the limit's worth of attempts were recorded"
        );

        // Bucket 0 is reserved: a real project's budget is untouched by the
        // flood above.
        assert!(limiter.check(1, Some(1)).await);
    }

    #[tokio::test]
    async fn none_and_non_positive_limits_fail_open() {
        let limiter = IngestRateLimiter::new();
        for _ in 0..100 {
            assert!(limiter.check(1, None).await);
            assert!(limiter.check(1, Some(0)).await);
            assert!(limiter.check(1, Some(-1)).await);
        }
    }
}
