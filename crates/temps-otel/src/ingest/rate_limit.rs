//! Per-project rate limiter for OTel ingest.
//!
//! Uses a sliding window counter per project to limit ingest volume.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per-project rate limiter using sliding window counters.
pub struct RateLimiter {
    /// Max requests per window per project.
    max_requests: u32,
    /// Window duration.
    window: Duration,
    /// Per-project counters.
    counters: Mutex<HashMap<i32, WindowCounter>>,
}

struct WindowCounter {
    count: u32,
    window_start: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// # Arguments
    /// * `max_requests` - Maximum requests allowed per window per project.
    /// * `window` - Duration of the sliding window.
    pub fn new(max_requests: u32, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Check if the request should be allowed, incrementing the counter.
    ///
    /// Returns `true` if allowed, `false` if rate limit exceeded.
    pub fn check_and_increment(&self, project_id: i32) -> bool {
        let now = Instant::now();
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());

        let counter = counters.entry(project_id).or_insert(WindowCounter {
            count: 0,
            window_start: now,
        });

        // Reset window if expired
        if now.duration_since(counter.window_start) >= self.window {
            counter.count = 0;
            counter.window_start = now;
        }

        if counter.count >= self.max_requests {
            return false;
        }

        counter.count += 1;
        true
    }

    /// Get current count for a project (for observability).
    pub fn current_count(&self, project_id: i32) -> u32 {
        let counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        counters.get(&project_id).map(|c| c.count).unwrap_or(0)
    }

    /// Clean up expired windows to prevent memory leaks.
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut counters = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        counters.retain(|_, c| now.duration_since(c.window_start) < self.window * 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        for _ in 0..5 {
            assert!(limiter.check_and_increment(1));
        }
    }

    #[test]
    fn test_rate_limiter_rejects_over_limit() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.check_and_increment(1));
        assert!(limiter.check_and_increment(1));
        assert!(limiter.check_and_increment(1));
        assert!(!limiter.check_and_increment(1));
    }

    #[test]
    fn test_rate_limiter_per_project_isolation() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_and_increment(1));
        assert!(!limiter.check_and_increment(1));
        // Different project should still be allowed
        assert!(limiter.check_and_increment(2));
    }

    #[test]
    fn test_current_count() {
        let limiter = RateLimiter::new(10, Duration::from_secs(60));
        assert_eq!(limiter.current_count(1), 0);
        limiter.check_and_increment(1);
        limiter.check_and_increment(1);
        assert_eq!(limiter.current_count(1), 2);
    }

    #[test]
    fn test_cleanup_expired() {
        let limiter = RateLimiter::new(10, Duration::from_millis(1));
        limiter.check_and_increment(1);
        std::thread::sleep(Duration::from_millis(5));
        limiter.cleanup_expired();
        // After cleanup, counter should have been removed
        assert_eq!(limiter.current_count(1), 0);
    }
}
