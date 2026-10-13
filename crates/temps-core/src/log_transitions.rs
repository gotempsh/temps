// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Log recurring conditions on state transitions instead of on every tick.
//!
//! Background loops (alert evaluation, container health polling, route
//! reconciliation, facet backfill, ...) run every few seconds. When something
//! they depend on is broken, logging the same failure on every tick buries the
//! one line that matters — when it started — under thousands of identical
//! copies, and inflates the per-module ERROR counts that anonymous telemetry
//! uses to find where installs break.
//!
//! [`FailureLatch`] (one condition) and [`KeyedFailureLatch`] (one condition
//! per key, e.g. per rule or per container) turn a stream of observations into
//! transitions:
//!
//! - the first failure after a healthy state is [`FailureLog::Started`]: log it
//!   at its real level;
//! - further failures are [`FailureLog::Suppressed`] (log at DEBUG at most),
//!   except that once per reminder interval a [`FailureLog::Reminder`] carries
//!   the consecutive count so a long outage is never silent;
//! - the first success afterwards reports how many failures it ended, so the
//!   caller can log the recovery.
//!
//! The latches never decide the level themselves. A database outage is still
//! an ERROR; it is just reported once per transition (plus reminders) rather
//! than once per tick.
//!
//! These are control-plane helpers guarded by a short `std::sync::Mutex`;
//! do not use them on a per-request hot path.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often a condition that is still failing is re-reported at its full
/// level. Long enough to collapse a per-tick loop to a handful of lines per
/// day, short enough that an operator tailing logs sees an ongoing outage.
pub const DEFAULT_REMINDER_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// What the caller should do with one failed observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureLog {
    /// First failure after a healthy (or never observed) state. Log it at the
    /// condition's real level.
    Started,
    /// Still failing and the reminder interval has elapsed since the last
    /// logged line. Log it at the real level, including the count.
    Reminder {
        /// Failures observed in a row, including this one.
        consecutive: u64,
    },
    /// Still failing within the reminder interval. Log at DEBUG at most.
    Suppressed {
        /// Failures observed in a row, including this one.
        consecutive: u64,
    },
}

impl FailureLog {
    /// Whether this observation should be logged at the condition's real
    /// level (`Started` or `Reminder`).
    pub fn should_log(&self) -> bool {
        !matches!(self, Self::Suppressed { .. })
    }

    /// Failures observed in a row, including this one.
    pub fn consecutive(&self) -> u64 {
        match self {
            Self::Started => 1,
            Self::Reminder { consecutive } | Self::Suppressed { consecutive } => *consecutive,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FailureState {
    consecutive: u64,
    last_logged: Instant,
}

impl FailureState {
    fn started(now: Instant) -> Self {
        Self {
            consecutive: 1,
            last_logged: now,
        }
    }

    fn advance(&mut self, now: Instant, reminder: Duration) -> FailureLog {
        self.consecutive = self.consecutive.saturating_add(1);
        if now.saturating_duration_since(self.last_logged) >= reminder {
            self.last_logged = now;
            FailureLog::Reminder {
                consecutive: self.consecutive,
            }
        } else {
            FailureLog::Suppressed {
                consecutive: self.consecutive,
            }
        }
    }
}

/// Tracks a single recurring condition.
///
/// `const`-constructible so it can live in a `static` for process-wide
/// conditions (e.g. "the GeoIP database cannot answer lookups").
#[derive(Debug)]
pub struct FailureLatch {
    state: Mutex<Option<FailureState>>,
    reminder: Duration,
}

impl Default for FailureLatch {
    fn default() -> Self {
        Self::new(DEFAULT_REMINDER_INTERVAL)
    }
}

impl FailureLatch {
    /// A latch that re-reports an ongoing failure every `reminder`.
    pub const fn new(reminder: Duration) -> Self {
        Self {
            state: Mutex::new(None),
            reminder,
        }
    }

    /// Record a failed observation.
    pub fn record_failure(&self) -> FailureLog {
        self.record_failure_at(Instant::now())
    }

    /// [`Self::record_failure`] with an explicit clock, for tests.
    pub fn record_failure_at(&self, now: Instant) -> FailureLog {
        let mut state = lock(&self.state);
        match state.as_mut() {
            Some(failing) => failing.advance(now, self.reminder),
            None => {
                *state = Some(FailureState::started(now));
                FailureLog::Started
            }
        }
    }

    /// Record a successful observation. Returns the number of consecutive
    /// failures this success ended, or `None` if the condition was already
    /// healthy (nothing to log).
    pub fn record_success(&self) -> Option<u64> {
        lock(&self.state).take().map(|failing| failing.consecutive)
    }

    /// Whether the condition is currently failing.
    pub fn is_failing(&self) -> bool {
        lock(&self.state).is_some()
    }
}

/// Tracks one recurring condition per key (rule ID, container ID, ...).
///
/// Only failing keys are stored, so memory is bounded by the number of keys
/// failing at once. Callers whose keys can disappear (a deleted rule that was
/// failing) should call [`Self::retain`] with the live key set once per cycle.
#[derive(Debug)]
pub struct KeyedFailureLatch<K> {
    states: Mutex<HashMap<K, FailureState>>,
    reminder: Duration,
}

impl<K: Eq + Hash> Default for KeyedFailureLatch<K> {
    fn default() -> Self {
        Self::new(DEFAULT_REMINDER_INTERVAL)
    }
}

impl<K: Eq + Hash> KeyedFailureLatch<K> {
    /// A keyed latch that re-reports each ongoing failure every `reminder`.
    pub fn new(reminder: Duration) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            reminder,
        }
    }

    /// Record a failed observation for `key`.
    pub fn record_failure(&self, key: K) -> FailureLog {
        self.record_failure_at(key, Instant::now())
    }

    /// [`Self::record_failure`] with an explicit clock, for tests.
    pub fn record_failure_at(&self, key: K, now: Instant) -> FailureLog {
        let mut states = lock(&self.states);
        match states.get_mut(&key) {
            Some(failing) => failing.advance(now, self.reminder),
            None => {
                states.insert(key, FailureState::started(now));
                FailureLog::Started
            }
        }
    }

    /// Record a failure with a fixed memory budget. Existing keys keep their
    /// transition state; at capacity the oldest logged key is evicted.
    pub fn record_failure_bounded(&self, key: K, capacity: usize) -> FailureLog
    where
        K: Clone,
    {
        let now = Instant::now();
        let mut states = lock(&self.states);
        if let Some(failing) = states.get_mut(&key) {
            return failing.advance(now, self.reminder);
        }
        if capacity == 0 {
            return FailureLog::Started;
        }
        if states.len() >= capacity {
            if let Some(oldest) = states
                .iter()
                .min_by_key(|(_, state)| state.last_logged)
                .map(|(key, _)| key.clone())
            {
                states.remove(&oldest);
            }
        }
        states.insert(key, FailureState::started(now));
        FailureLog::Started
    }

    /// Record a successful observation for `key`. Returns the number of
    /// consecutive failures it ended, or `None` if `key` was not failing.
    pub fn record_success(&self, key: &K) -> Option<u64> {
        lock(&self.states)
            .remove(key)
            .map(|failing| failing.consecutive)
    }

    /// Whether `key` is currently failing.
    pub fn is_failing(&self, key: &K) -> bool {
        lock(&self.states).contains_key(key)
    }

    /// Drop the state of every key for which `keep` returns `false`.
    pub fn retain(&self, mut keep: impl FnMut(&K) -> bool) {
        lock(&self.states).retain(|key, _| keep(key));
    }

    /// Number of keys currently failing.
    pub fn failing_count(&self) -> usize {
        lock(&self.states).len()
    }
}

/// A poisoned lock only means another thread panicked mid-update of a log
/// throttle; the worst outcome of reusing its state is one extra or one
/// missing log line, which beats propagating the panic.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REMINDER: Duration = Duration::from_secs(60);
    #[test]
    fn bounded_latch_suppresses_repeats_recovers_and_limits_memory() {
        let latch = KeyedFailureLatch::new(REMINDER);
        assert_eq!(latch.record_failure_bounded(1, 2), FailureLog::Started);
        assert!(!latch.record_failure_bounded(1, 2).should_log());
        assert_eq!(latch.record_success(&1), Some(2));
        assert_eq!(latch.record_failure_bounded(1, 2), FailureLog::Started);
        latch.record_failure_bounded(2, 2);
        latch.record_failure_bounded(3, 2);
        assert_eq!(latch.failing_count(), 2);
        assert!(!latch.is_failing(&1));
    }

    #[test]
    fn first_failure_starts_and_repeats_are_suppressed() {
        let latch = FailureLatch::new(REMINDER);
        let t0 = Instant::now();

        assert_eq!(latch.record_failure_at(t0), FailureLog::Started);
        assert_eq!(
            latch.record_failure_at(t0 + Duration::from_secs(30)),
            FailureLog::Suppressed { consecutive: 2 }
        );
        assert_eq!(
            latch.record_failure_at(t0 + Duration::from_secs(59)),
            FailureLog::Suppressed { consecutive: 3 }
        );
        assert!(latch.is_failing());
    }

    #[test]
    fn ongoing_failure_is_re_reported_once_per_reminder_interval() {
        let latch = FailureLatch::new(REMINDER);
        let t0 = Instant::now();

        assert_eq!(latch.record_failure_at(t0), FailureLog::Started);
        let reminder = latch.record_failure_at(t0 + REMINDER);
        assert_eq!(reminder, FailureLog::Reminder { consecutive: 2 });
        assert!(reminder.should_log());
        // The reminder resets the window: the next tick is quiet again.
        assert_eq!(
            latch.record_failure_at(t0 + REMINDER + Duration::from_secs(1)),
            FailureLog::Suppressed { consecutive: 3 }
        );
    }

    #[test]
    fn success_reports_recovery_once_and_rearms_the_latch() {
        let latch = FailureLatch::new(REMINDER);
        let t0 = Instant::now();

        assert_eq!(latch.record_success(), None, "healthy stays silent");
        latch.record_failure_at(t0);
        latch.record_failure_at(t0);
        latch.record_failure_at(t0);
        assert_eq!(latch.record_success(), Some(3));
        assert_eq!(latch.record_success(), None, "recovery is reported once");
        assert!(!latch.is_failing());
        // A new outage is a new transition and must be logged again.
        assert_eq!(latch.record_failure_at(t0), FailureLog::Started);
    }

    #[test]
    fn static_latch_is_const_constructible() {
        static LATCH: FailureLatch = FailureLatch::new(DEFAULT_REMINDER_INTERVAL);
        assert!(!LATCH.is_failing());
    }

    #[test]
    fn failure_log_helpers() {
        assert!(FailureLog::Started.should_log());
        assert_eq!(FailureLog::Started.consecutive(), 1);
        assert!(!FailureLog::Suppressed { consecutive: 4 }.should_log());
        assert_eq!(FailureLog::Suppressed { consecutive: 4 }.consecutive(), 4);
        assert_eq!(FailureLog::Reminder { consecutive: 9 }.consecutive(), 9);
    }

    #[test]
    fn keyed_latch_tracks_keys_independently() {
        let latch = KeyedFailureLatch::new(REMINDER);
        let t0 = Instant::now();

        assert_eq!(latch.record_failure_at(1, t0), FailureLog::Started);
        assert_eq!(latch.record_failure_at(2, t0), FailureLog::Started);
        assert_eq!(
            latch.record_failure_at(1, t0),
            FailureLog::Suppressed { consecutive: 2 }
        );
        assert_eq!(latch.record_success(&2), Some(1));
        assert!(latch.is_failing(&1));
        assert!(!latch.is_failing(&2));
        assert_eq!(latch.failing_count(), 1);
    }

    #[test]
    fn keyed_latch_retain_forgets_vanished_keys() {
        let latch = KeyedFailureLatch::new(REMINDER);
        let t0 = Instant::now();
        for key in 0..10 {
            latch.record_failure_at(key, t0);
        }
        latch.retain(|key| key % 2 == 0);
        assert_eq!(latch.failing_count(), 5);
        assert!(!latch.is_failing(&1));
        // A forgotten key that fails again is a fresh transition.
        assert_eq!(latch.record_failure_at(1, t0), FailureLog::Started);
    }
}
