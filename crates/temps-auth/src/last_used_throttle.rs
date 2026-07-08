//! Per-credential throttle for `last_used_at` bookkeeping writes.
//!
//! `ApiKeyService::validate_api_key` and `DeploymentTokenValidationService::validate_token`
//! run on every authenticated request platform-wide and each write
//! `last_used_at = NOW()` unconditionally. That timestamp is only ever shown
//! as an informational "last used" display — it doesn't need per-request
//! freshness — so paying for a DB write on every single request wastes load
//! proportional to all authenticated traffic. This throttle lets callers skip
//! the write entirely when one already happened recently for the same
//! credential id, rather than just moving the write off the critical path.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Minimum spacing between `last_used_at` writes for a single credential.
/// `last_used_at` is a UI/audit display value, not a security or billing
/// gate, so this can be far looser than e.g. the OTel quota cache's 30s
/// window (`crates/temps-otel/src/ingest/quota_cache.rs`).
pub const LAST_USED_UPDATE_INTERVAL: Duration = Duration::from_secs(300);

/// Tracks the last time `last_used_at` was written for each credential id
/// (an `api_keys.id` or `deployment_tokens.id`), so repeat validations
/// within `interval` can skip the write.
pub struct LastUsedThrottle {
    interval: Duration,
    last_updates: Mutex<HashMap<i32, Instant>>,
}

impl LastUsedThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_updates: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the caller should write `last_used_at` now (and
    /// records that a write is about to happen), `false` if one already
    /// happened within `interval` and this call can skip the DB write
    /// entirely. Check-and-mark happens under one lock acquisition so
    /// concurrent callers for the same id can't both decide to write.
    pub fn should_update(&self, id: i32) -> bool {
        let now = Instant::now();
        let mut last_updates = self.last_updates.lock().unwrap_or_else(|e| e.into_inner());

        match last_updates.get(&id) {
            Some(last) if now.duration_since(*last) < self.interval => false,
            _ => {
                last_updates.insert(id, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_call_updates() {
        let throttle = LastUsedThrottle::new(Duration::from_secs(60));
        assert!(throttle.should_update(1));
    }

    #[test]
    fn test_immediate_repeat_call_skips() {
        let throttle = LastUsedThrottle::new(Duration::from_secs(60));
        assert!(throttle.should_update(1));
        assert!(!throttle.should_update(1));
    }

    #[test]
    fn test_updates_again_after_interval_elapses() {
        let throttle = LastUsedThrottle::new(Duration::from_millis(1));
        assert!(throttle.should_update(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(throttle.should_update(1));
    }

    #[test]
    fn test_distinct_ids_are_independent() {
        let throttle = LastUsedThrottle::new(Duration::from_secs(60));
        assert!(throttle.should_update(1));
        assert!(throttle.should_update(2));
        assert!(!throttle.should_update(1));
        assert!(!throttle.should_update(2));
    }
}
