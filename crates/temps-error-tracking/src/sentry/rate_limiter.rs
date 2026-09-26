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

/// Result of [`IngestRateLimiter::check_reserving`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The project has no configured limit; nothing was reserved because
    /// there is nothing to reserve against.
    Unlimited,
    /// A slot was reserved at this instant. Pass to [`IngestRateLimiter::release`]
    /// to undo it if the reservation turns out to be against the wrong
    /// project.
    Reserved(Instant),
    /// The bucket is at capacity; the request must be rejected.
    Denied,
}

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

    /// Like [`Self::check`], but the caller may not yet know the *final*
    /// project a request will be billed to — the tunnel handler resolves a
    /// tentative project from `Host` before it has decompressed the body to
    /// read an SDK-embedded DSN, which can name a different project.
    ///
    /// This still reserves a slot (unlike a non-consuming peek, which lets
    /// unbounded concurrent requests all observe the same remaining slot and
    /// all proceed into the expensive work behind the gate — the exact
    /// mistake this replaces), but returns the marker for that reservation
    /// so the caller can [`Self::release`] it precisely if the tentative
    /// project turns out to be wrong, then [`Self::check`] the real one.
    pub async fn check_reserving(
        &self,
        project_id: i32,
        limit_per_minute: Option<i32>,
    ) -> Admission {
        let limit = match limit_per_minute {
            Some(limit) if limit > 0 => limit as usize,
            _ => return Admission::Unlimited,
        };

        let now = Instant::now();
        let window_start = now - WINDOW;

        let mut entries = self.entries.lock().await;
        let timestamps = entries.entry(project_id).or_default();
        timestamps.retain(|t| *t > window_start);

        if timestamps.len() >= limit {
            return Admission::Denied;
        }

        timestamps.push(now);
        Admission::Reserved(now)
    }

    /// Undo a reservation made by [`Self::check_reserving`] for `project_id`.
    ///
    /// Removes the exact `marker` timestamp rather than e.g. the bucket's
    /// last entry, so it cannot accidentally free a *different* concurrent
    /// request's slot — `Instant` has sub-microsecond resolution on every
    /// platform this runs on, so two reservations for the same project
    /// colliding on the same instant is not a realistic concern.
    pub async fn release(&self, project_id: i32, marker: Instant) {
        let mut entries = self.entries.lock().await;
        if let Some(timestamps) = entries.get_mut(&project_id) {
            if let Some(pos) = timestamps.iter().position(|t| *t == marker) {
                timestamps.remove(pos);
            }
        }
    }

    /// Read-only inspection of `project_id`'s remaining budget, without
    /// consuming a slot. For tests and observability only — **not** an
    /// admission gate: a peek that says "allowed" reserves nothing, so
    /// concurrent callers can all observe the same remaining slot and all
    /// proceed. [`Self::check_reserving`] is what gates expensive work.
    #[cfg(test)]
    pub(crate) async fn peek(&self, project_id: i32, limit_per_minute: Option<i32>) -> bool {
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

    /// `check_reserving` must actually consume a slot — the defect this
    /// replaces (a non-consuming peek) let unbounded concurrent callers all
    /// observe "still room" and all proceed, because nothing about a peek
    /// changes what the next peek sees.
    #[tokio::test]
    async fn check_reserving_actually_reserves_a_slot() {
        let limiter = IngestRateLimiter::new();

        assert!(matches!(
            limiter.check_reserving(1, Some(1)).await,
            Admission::Reserved(_)
        ));
        // The single slot is now taken: a second reservation attempt for the
        // same project, in the same window, must be denied.
        assert_eq!(limiter.check_reserving(1, Some(1)).await, Admission::Denied);
    }

    #[tokio::test]
    async fn check_reserving_is_unlimited_for_none_or_non_positive_limits() {
        let limiter = IngestRateLimiter::new();
        assert_eq!(limiter.check_reserving(1, None).await, Admission::Unlimited);
        assert_eq!(
            limiter.check_reserving(1, Some(0)).await,
            Admission::Unlimited
        );
    }

    /// `release` frees exactly the marker it is given, and nothing else —
    /// it must not free a slot some other concurrent request holds.
    #[tokio::test]
    async fn release_frees_only_the_given_marker() {
        let limiter = IngestRateLimiter::new();

        let first = match limiter.check_reserving(1, Some(2)).await {
            Admission::Reserved(marker) => marker,
            other => panic!("expected Reserved, got {other:?}"),
        };
        let second = match limiter.check_reserving(1, Some(2)).await {
            Admission::Reserved(marker) => marker,
            other => panic!("expected Reserved, got {other:?}"),
        };
        // Bucket is now full (2/2).
        assert_eq!(limiter.check_reserving(1, Some(2)).await, Admission::Denied);

        limiter.release(1, first).await;
        // One slot freed: exactly one more reservation succeeds.
        assert!(matches!(
            limiter.check_reserving(1, Some(2)).await,
            Admission::Reserved(_)
        ));
        assert_eq!(limiter.check_reserving(1, Some(2)).await, Admission::Denied);

        limiter.release(1, second).await;
        assert!(matches!(
            limiter.check_reserving(1, Some(2)).await,
            Admission::Reserved(_)
        ));
    }

    #[tokio::test]
    async fn release_of_an_unknown_marker_is_a_harmless_no_op() {
        let limiter = IngestRateLimiter::new();
        limiter.release(1, Instant::now()).await;
        assert!(matches!(
            limiter.check_reserving(1, Some(1)).await,
            Admission::Reserved(_)
        ));
    }
}
