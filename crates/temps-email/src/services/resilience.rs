//! In-memory per-provider circuit breaker and rate limiter for the email
//! send path. This is control-plane code — email sends are nowhere near
//! proxy/ingest hot-path volume — so a `Mutex<HashMap<..>>` keyed by the
//! small, bounded set of configured provider ids is fine here (see
//! CLAUDE.md's hot-path-vs-control-plane distinction).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const FAILURE_THRESHOLD: u32 = 5;
const OPEN_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Default)]
struct CircuitState {
    consecutive_failures: u32,
    /// `Some` while the circuit is open (failing fast). Cleared once the
    /// cooldown elapses to admit a half-open trial.
    opened_at: Option<Instant>,
}

/// Consecutive-failure circuit breaker, one state machine per provider id.
/// Trips open after `FAILURE_THRESHOLD` consecutive failures and fails fast
/// for `OPEN_COOLDOWN` before admitting trial requests again.
///
/// Simplification: once the cooldown elapses, *every* concurrent caller is
/// admitted (not just a single half-open trial) until the next recorded
/// failure re-opens the circuit. At email-send concurrency this is an
/// acceptable trade for staying lock-free across the actual send call; a
/// strict single-trial half-open state would need an in-flight marker.
pub struct ProviderCircuitBreaker {
    states: Mutex<HashMap<i32, CircuitState>>,
}

impl Default for ProviderCircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderCircuitBreaker {
    pub fn new() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a send attempt against this provider should be allowed right
    /// now. `false` means "skip this provider, try the next one in the
    /// failover chain" — not a hard error.
    pub fn allow(&self, provider_id: i32) -> bool {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states.entry(provider_id).or_default();
        match state.opened_at {
            None => true,
            Some(opened_at) if opened_at.elapsed() >= OPEN_COOLDOWN => {
                state.opened_at = None;
                true
            }
            Some(_) => false,
        }
    }

    pub fn record_success(&self, provider_id: i32) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.entry(provider_id).or_default().consecutive_failures = 0;
    }

    pub fn record_failure(&self, provider_id: i32) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let state = states.entry(provider_id).or_default();
        state.consecutive_failures += 1;
        if state.consecutive_failures >= FAILURE_THRESHOLD {
            state.opened_at = Some(Instant::now());
        }
    }
}

/// Sliding-window per-provider send rate limiter, mirroring the pattern in
/// `temps_auth::rate_limit::AuthRateLimiter`. The limit itself
/// (`email_providers.rate_limit_per_minute`) is operator-configured per
/// provider rather than a global constant, so a slow SMTP relay and a
/// high-throughput SES account can coexist.
pub struct ProviderRateLimiter {
    windows: Mutex<HashMap<i32, Vec<Instant>>>,
}

impl Default for ProviderRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRateLimiter {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` and records the attempt if the provider is under its
    /// per-minute cap; returns `false` without recording anything if not, so
    /// a denied attempt doesn't consume capacity it never used and the
    /// caller is free to try the next provider in the chain.
    /// `limit_per_minute: None` means unlimited (always allowed).
    pub fn try_acquire(&self, provider_id: i32, limit_per_minute: Option<i32>) -> bool {
        let Some(limit) = limit_per_minute else {
            return true;
        };
        if limit <= 0 {
            return false;
        }

        let now = Instant::now();
        let window_start = now - Duration::from_secs(60);

        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let timestamps = windows.entry(provider_id).or_default();
        timestamps.retain(|t| *t > window_start);

        if timestamps.len() >= limit as usize {
            false
        } else {
            timestamps.push(now);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_allows_until_threshold() {
        let cb = ProviderCircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            cb.record_failure(1);
            assert!(cb.allow(1), "should still be closed below threshold");
        }
        cb.record_failure(1);
        assert!(!cb.allow(1), "should trip open at threshold");
    }

    #[test]
    fn circuit_success_resets_failure_count() {
        let cb = ProviderCircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            cb.record_failure(1);
        }
        cb.record_success(1);
        cb.record_failure(1);
        assert!(cb.allow(1), "one failure after a reset shouldn't trip it");
    }

    #[test]
    fn circuit_is_per_provider() {
        let cb = ProviderCircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure(1);
        }
        assert!(!cb.allow(1));
        assert!(cb.allow(2), "a different provider's circuit is independent");
    }

    #[test]
    fn rate_limiter_unlimited_when_none() {
        let rl = ProviderRateLimiter::new();
        for _ in 0..1000 {
            assert!(rl.try_acquire(1, None));
        }
    }

    #[test]
    fn rate_limiter_denies_over_cap() {
        let rl = ProviderRateLimiter::new();
        assert!(rl.try_acquire(1, Some(2)));
        assert!(rl.try_acquire(1, Some(2)));
        assert!(!rl.try_acquire(1, Some(2)), "third attempt exceeds the cap of 2/min");
    }

    #[test]
    fn rate_limiter_zero_cap_denies_everything() {
        let rl = ProviderRateLimiter::new();
        assert!(!rl.try_acquire(1, Some(0)));
    }

    #[test]
    fn rate_limiter_is_per_provider() {
        let rl = ProviderRateLimiter::new();
        assert!(rl.try_acquire(1, Some(1)));
        assert!(!rl.try_acquire(1, Some(1)));
        assert!(rl.try_acquire(2, Some(1)), "a different provider has its own budget");
    }
}
