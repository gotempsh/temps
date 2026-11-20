// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Failed unlock attempts per client and page.
//!
//! [`MAX_FAILURES`] wrong passwords from one client on one page lock that
//! client out of that page for the rest of a [`FAILURE_WINDOW`]. Memory is
//! bounded by [`MAX_TRACKED`]: when the table is full of live entries, a new
//! client is refused until entries expire. That makes a flood of addresses
//! able to delay unlocking, never to guess faster.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_FAILURES: u32 = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
pub const MAX_TRACKED: usize = 10_000;

#[derive(Default)]
pub struct UnlockLimiter {
    /// (page, client) -> (window start, failures in it).
    failures: Mutex<HashMap<(i32, String), (Instant, u32)>>,
}

impl UnlockLimiter {
    /// `Err(retry after)` when `client` may not try `status_page_id` now.
    pub fn check(&self, status_page_id: i32, client: &str, now: Instant) -> Result<(), Duration> {
        // A poisoned lock only means a panic mid-update of a counter.
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let key = (status_page_id, client.to_string());
        if let Some((start, count)) = failures.get(&key) {
            let elapsed = now.saturating_duration_since(*start);
            if elapsed < FAILURE_WINDOW && *count >= MAX_FAILURES {
                return Err(FAILURE_WINDOW - elapsed);
            }
            return Ok(());
        }
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, (start, _)| now.saturating_duration_since(*start) < FAILURE_WINDOW);
            if failures.len() >= MAX_TRACKED {
                return Err(Duration::from_secs(60));
            }
        }
        Ok(())
    }

    pub fn record_failure(&self, status_page_id: i32, client: &str, now: Instant) {
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, (start, _)| now.saturating_duration_since(*start) < FAILURE_WINDOW);
        }
        let entry = failures
            .entry((status_page_id, client.to_string()))
            .or_insert((now, 0));
        if now.saturating_duration_since(entry.0) >= FAILURE_WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
    }

    pub fn clear(&self, status_page_id: i32, client: &str) {
        let mut failures = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        failures.remove(&(status_page_id, client.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_lock_out_one_client_on_one_page_for_the_window() {
        let limiter = UnlockLimiter::default();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            assert!(limiter.check(1, "192.0.2.1", t0).is_ok());
            limiter.record_failure(1, "192.0.2.1", t0);
        }
        let retry = limiter.check(1, "192.0.2.1", t0).expect_err("locked out");
        assert_eq!(retry, FAILURE_WINDOW);
        assert!(limiter.check(2, "192.0.2.1", t0).is_ok(), "another page");
        assert!(limiter.check(1, "192.0.2.2", t0).is_ok(), "another client");
        assert!(limiter.check(1, "192.0.2.1", t0 + FAILURE_WINDOW).is_ok());
        limiter.clear(1, "192.0.2.1");
        assert!(limiter.check(1, "192.0.2.1", t0).is_ok());
    }
}
