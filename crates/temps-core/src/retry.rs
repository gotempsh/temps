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
    /// Base delay between retries. Actual delay is `base_delay * 2^attempt`.
    pub base_delay: Duration,
    /// Maximum delay cap to prevent unbounded waits.
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
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

    /// Execute an async operation with retry logic.
    ///
    /// The operation is retried on failure up to `max_attempts` times with
    /// exponential backoff. The error type must implement `Display` for logging.
    ///
    /// Returns the result of the first successful attempt, or the last error
    /// if all attempts fail.
    pub async fn retry<F, Fut, T, E>(&self, mut operation: F) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        let mut last_error: Option<E> = None;

        for attempt in 0..self.max_attempts {
            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        debug!("Operation succeeded on attempt {}", attempt + 1);
                    }
                    return Ok(result);
                }
                Err(e) => {
                    let is_last_attempt = attempt + 1 >= self.max_attempts;
                    if is_last_attempt {
                        warn!(
                            "Operation failed on final attempt {}/{}: {}",
                            attempt + 1,
                            self.max_attempts,
                            e
                        );
                        last_error = Some(e);
                    } else {
                        let delay = self.compute_delay(attempt);
                        warn!(
                            "Operation failed on attempt {}/{}, retrying in {:?}: {}",
                            attempt + 1,
                            self.max_attempts,
                            delay,
                            e
                        );
                        tokio::time::sleep(delay).await;
                        last_error = Some(e);
                    }
                }
            }
        }

        Err(last_error.expect("retry loop must have at least one attempt"))
    }

    /// Compute the delay for the given attempt using exponential backoff.
    ///
    /// `pub` (not just crate-internal) so callers that need early-exit
    /// semantics `retry()` doesn't support — e.g. stopping immediately on a
    /// terminal error instead of retrying it — can hand-roll their own loop
    /// while still reusing this config's backoff math instead of duplicating
    /// it.
    pub fn compute_delay(&self, attempt: u32) -> Duration {
        let delay = self.base_delay.saturating_mul(1 << attempt);
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

    #[test]
    fn test_compute_delay_exponential_backoff() {
        let config = RetryConfig {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
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
        };

        assert_eq!(config.compute_delay(0), Duration::from_secs(1));
        assert_eq!(config.compute_delay(1), Duration::from_secs(2));
        assert_eq!(config.compute_delay(2), Duration::from_secs(3)); // capped
        assert_eq!(config.compute_delay(3), Duration::from_secs(3)); // capped
    }
}
