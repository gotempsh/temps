//! In-memory sliding-window rate limiter for Sentry ingest, keyed by project id.
//!
//! Cardinality is bounded by the number of projects with error tracking
//! enabled — not by visitor/IP, which is unbounded — so a plain
//! `Mutex<HashMap>` is safe to hold here even though ingest is a public,
//! unauthenticated-by-credential surface (the tunnel route resolves the
//! project from `Host` with no DSN check). This is not the proxy hot path;
//! it runs at the same order of magnitude as `temps-auth`'s per-IP limiter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const WINDOW: Duration = Duration::from_secs(60);

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
    async fn none_and_non_positive_limits_fail_open() {
        let limiter = IngestRateLimiter::new();
        for _ in 0..100 {
            assert!(limiter.check(1, None).await);
            assert!(limiter.check(1, Some(0)).await);
            assert!(limiter.check(1, Some(-1)).await);
        }
    }
}
