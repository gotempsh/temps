// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Generic retry utility for async operations with exponential backoff.
//!
//! Provides a configurable retry mechanism for transient failures when calling
//! external services (Git providers, DNS APIs, etc.).
//!
//! # Example
//!
//! ```rust,ignore
//! use temps_core::retry::RetryConfig;
//!
//! let config = RetryConfig::default(); // 3 attempts, 1s base delay
//!
//! let result = config.retry(|| async {
//!     client.get("https://api.github.com/repos/owner/repo")
//!         .send()
//!         .await
//!         .map_err(|e| e.to_string())
//! }).await;
//! ```

use std::fmt;
use std::future::Future;
use std::time::Duration;
use tracing::{debug, warn};

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the first try).
    pub max_attempts: u32,
    /// Base delay between retries. Actual delay is
    /// `base_delay * backoff_factor^attempt`.
    pub base_delay: Duration,
    /// Maximum delay cap to prevent unbounded waits.
    pub max_delay: Duration,
    /// Growth of the delay from one retry to the next (2 doubles it).
    pub backoff_factor: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
            backoff_factor: 2,
        }
    }
}

impl RetryConfig {
    /// Create a new retry config with the given max attempts.
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            ..Default::default()
        }
    }

    /// Set the base delay for exponential backoff.
    pub fn with_base_delay(mut self, delay: Duration) -> Self {
        self.base_delay = delay;
        self
    }

    /// Set the maximum delay cap.
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set how much the delay grows from one retry to the next (default 2).
    pub fn with_backoff_factor(mut self, factor: u32) -> Self {
        self.backoff_factor = factor;
        self
    }

    /// Execute an async operation with retry logic.
    ///
    /// The operation is retried on failure up to `max_attempts` times with
    /// exponential backoff. The error type must implement `Display` for logging.
    ///
    /// Returns the result of the first successful attempt, or the last error
    /// if all attempts fail. A `max_attempts` of `0` is treated as `1`: the
    /// operation always runs at least once.
    pub async fn retry<F, Fut, T, E>(&self, operation: F) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        self.retry_if(operation, |_| true).await
    }

    /// Like [`retry`](Self::retry), but only errors for which `is_retryable`
    /// returns true are retried; any other error is returned at once.
    pub async fn retry_if<F, Fut, T, E, P>(&self, mut operation: F, is_retryable: P) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: fmt::Display,
        P: Fn(&E) -> bool,
    {
        let max_attempts = self.max_attempts.max(1);
        let mut attempt: u32 = 0;

        loop {
            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        debug!("Operation succeeded on attempt {}", attempt + 1);
                    }
                    return Ok(result);
                }
                Err(e) => {
                    if !is_retryable(&e) {
                        debug!(
                            "Operation failed on attempt {}/{} with an error that is not retried: {}",
                            attempt + 1,
                            max_attempts,
                            e
                        );
                        return Err(e);
                    }
                    let is_last_attempt = attempt + 1 >= max_attempts;
                    if is_last_attempt {
                        warn!(
                            "Operation failed on final attempt {}/{}: {}",
                            attempt + 1,
                            max_attempts,
                            e
                        );
                        return Err(e);
                    }
                    let delay = self.compute_delay(attempt);
                    warn!(
                        "Operation failed on attempt {}/{}, retrying in {:?}: {}",
                        attempt + 1,
                        max_attempts,
                        delay,
                        e
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    /// Compute the delay for the given attempt using exponential backoff.
    ///
    /// `pub` (not just crate-internal) so callers that need early-exit
    /// semantics `retry()` doesn't support — e.g. stopping immediately on a
    /// terminal error instead of retrying it — can hand-roll their own loop
    /// while still reusing this config's backoff math instead of duplicating
    /// it.
    pub fn compute_delay(&self, attempt: u32) -> Duration {
        let delay = self
            .base_delay
            .saturating_mul(self.backoff_factor.saturating_pow(attempt));
        std::cmp::min(delay, self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_succeeds_first_attempt() {
        let config = RetryConfig::default();
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry(|| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok("success")
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_failures() {
        let config = RetryConfig::new(3).with_base_delay(Duration::from_millis(10));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry(|| {
                let count = count.clone();
                async move {
                    let attempt = count.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        Err("transient error".to_string())
                    } else {
                        Ok("success")
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_exhausts_all_attempts() {
        let config = RetryConfig::new(2).with_base_delay(Duration::from_millis(10));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry(|| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err("permanent error".to_string())
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "permanent error");
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_retry_zero_max_attempts_runs_once() {
        let config = RetryConfig::new(0).with_base_delay(Duration::from_millis(10));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry(|| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Err("permanent error".to_string())
                }
            })
            .await;

        assert_eq!(result.unwrap_err(), "permanent error");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_compute_delay_exponential_backoff() {
        let config = RetryConfig {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            backoff_factor: 2,
        };

        assert_eq!(config.compute_delay(0), Duration::from_millis(100));
        assert_eq!(config.compute_delay(1), Duration::from_millis(200));
        assert_eq!(config.compute_delay(2), Duration::from_millis(400));
        assert_eq!(config.compute_delay(3), Duration::from_millis(800));
    }

    #[test]
    fn test_compute_delay_caps_at_max() {
        let config = RetryConfig {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(3),
            backoff_factor: 2,
        };

        assert_eq!(config.compute_delay(0), Duration::from_secs(1));
        assert_eq!(config.compute_delay(1), Duration::from_secs(2));
        assert_eq!(config.compute_delay(2), Duration::from_secs(3)); // capped
        assert_eq!(config.compute_delay(3), Duration::from_secs(3)); // capped
    }

    #[test]
    fn test_compute_delay_with_backoff_factor() {
        // The log aggregator's seal schedule: 250 ms, 1 s, 4 s.
        let config = RetryConfig::new(4)
            .with_base_delay(Duration::from_millis(250))
            .with_backoff_factor(4)
            .with_max_delay(Duration::from_secs(4));

        assert_eq!(config.compute_delay(0), Duration::from_millis(250));
        assert_eq!(config.compute_delay(1), Duration::from_secs(1));
        assert_eq!(config.compute_delay(2), Duration::from_secs(4));
        assert_eq!(config.compute_delay(3), Duration::from_secs(4));
    }

    #[test]
    fn test_compute_delay_saturates_for_large_attempts() {
        // `2^attempt` no longer fits a u32 past attempt 31; the delay must
        // cap at `max_delay` instead of overflowing.
        let config = RetryConfig::default();

        assert_eq!(config.compute_delay(32), config.max_delay);
        assert_eq!(config.compute_delay(u32::MAX), config.max_delay);
    }

    #[tokio::test]
    async fn test_retry_if_retries_retryable_errors() {
        let config = RetryConfig::new(4).with_base_delay(Duration::from_millis(1));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry_if(
                || {
                    let count = count.clone();
                    async move {
                        if count.fetch_add(1, Ordering::SeqCst) < 2 {
                            Err("transient: connection reset".to_string())
                        } else {
                            Ok("success")
                        }
                    }
                },
                |e| e.starts_with("transient"),
            )
            .await;

        assert_eq!(result.unwrap(), "success");
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_if_returns_non_retryable_errors_at_once() {
        let config = RetryConfig::new(4).with_base_delay(Duration::from_millis(1));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry_if(
                || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Err("permanent: not found".to_string())
                    }
                },
                |e| e.starts_with("transient"),
            )
            .await;

        assert_eq!(result.unwrap_err(), "permanent: not found");
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_if_stops_at_max_attempts() {
        let config = RetryConfig::new(3).with_base_delay(Duration::from_millis(1));
        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();

        let result: Result<&str, String> = config
            .retry_if(
                || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Err("transient: timeout".to_string())
                    }
                },
                |e| e.starts_with("transient"),
            )
            .await;

        assert_eq!(result.unwrap_err(), "transient: timeout");
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }
}
