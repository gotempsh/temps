// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! How hard a challenge is (ADR 066, D6).
//!
//! The base follows why the visitor is challenged: [`FIRST`] for a list or
//! signal, [`RAISED`] in attack mode and after a spent budget, [`REPEATED`]
//! after repeated failed solutions. Under a spike in an environment's
//! traffic or errors, measured on this ingress against its last hour, up to
//! [`MAX_ADAPTIVE`] bits are added, never past [`MAX`].

use std::collections::HashMap;
use std::sync::Mutex;

use crate::identity::VisitorIdentity;

pub const FIRST: u8 = 18;
pub const RAISED: u8 = 20;
pub const REPEATED: u8 = 22;
pub const MAX: u8 = 24;
pub const MAX_ADAPTIVE: u8 = 4;

/// Failed solutions in one hour after which a visitor gets [`REPEATED`].
pub const REPEATED_FAILURES: u8 = 3;

/// Why a visitor is challenged, which sets the base difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeCause {
    /// A list, a fingerprint or a signal gave a `challenge` verdict.
    Verdict,
    /// The environment is in attack mode.
    Attack,
    /// The visitor's clearance used up its requests for this hour.
    BudgetSpent,
}

/// The difficulty for `cause`, raised after repeated failures and by
/// `adaptive` bits, capped at [`MAX`].
pub fn difficulty(cause: ChallengeCause, repeated_failures: bool, adaptive: u8) -> u8 {
    let base = if repeated_failures {
        REPEATED
    } else {
        match cause {
            ChallengeCause::Verdict => FIRST,
            ChallengeCause::Attack | ChallengeCause::BudgetSpent => RAISED,
        }
    };
    base.saturating_add(adaptive.min(MAX_ADAPTIVE)).min(MAX)
}

const MINUTES: usize = 60;
const SHARDS: usize = 16;

/// Requests below which a minute is too quiet to call a spike.
const SPIKE_MIN_REQUESTS: u32 = 120;
/// Requests below which a minute's error ratio says nothing.
const ERROR_MIN_REQUESTS: u32 = 60;

#[derive(Clone, Copy, Default)]
struct Minute {
    /// Minutes since the epoch this slot counts for.
    at: u64,
    requests: u32,
    errors: u32,
}

#[derive(Clone)]
struct EnvLoad {
    minutes: [Minute; MINUTES],
}

impl Default for EnvLoad {
    fn default() -> Self {
        Self {
            minutes: [Minute::default(); MINUTES],
        }
    }
}

impl EnvLoad {
    fn slot(&mut self, minute: u64) -> &mut Minute {
        let slot = &mut self.minutes[(minute % MINUTES as u64) as usize];
        if slot.at != minute {
            *slot = Minute {
                at: minute,
                ..Minute::default()
            };
        }
        slot
    }

    /// The last complete minute, and the average of the hour before it.
    fn compare(&self, minute: u64) -> Option<(Minute, f64, f64)> {
        let last = minute.checked_sub(1)?;
        let previous = self.minutes[(last % MINUTES as u64) as usize];
        if previous.at != last {
            return None;
        }
        let (mut requests, mut errors, mut counted) = (0u64, 0u64, 0u64);
        for slot in &self.minutes {
            if slot.at < last && last - slot.at < MINUTES as u64 {
                requests += u64::from(slot.requests);
                errors += u64::from(slot.errors);
                counted += 1;
            }
        }
        // Quiet minutes leave no slot: average over the whole hour.
        let window = (MINUTES as u64 - 1).max(counted) as f64;
        let average = requests as f64 / window;
        let error_ratio = if requests == 0 {
            0.0
        } else {
            errors as f64 / requests as f64
        };
        Some((previous, average, error_ratio))
    }
}

/// Each environment's request and error rate on this ingress, per minute
/// for the last hour.
pub struct LoadTracker {
    shards: Vec<Mutex<HashMap<i32, EnvLoad>>>,
}

impl Default for LoadTracker {
    fn default() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
        }
    }
}

impl LoadTracker {
    fn shard(&self, env_id: i32) -> std::sync::MutexGuard<'_, HashMap<i32, EnvLoad>> {
        let shard = &self.shards[(env_id as u32 as usize) % SHARDS];
        shard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counts one request to `env_id`.
    pub fn record_request(&self, env_id: i32, now: u64) {
        self.shard(env_id)
            .entry(env_id)
            .or_default()
            .slot(now / 60)
            .requests += 1;
    }

    /// Counts a response with `status` (only 5xx matter).
    pub fn record_status(&self, env_id: i32, status: u16, now: u64) {
        if (500..600).contains(&status) {
            self.shard(env_id)
                .entry(env_id)
                .or_default()
                .slot(now / 60)
                .errors += 1;
        }
    }

    /// Bits to add for `env_id` now: one per doubling past three times the
    /// hourly rate, and one more when the error ratio spiked.
    pub fn extra_bits(&self, env_id: i32, now: u64) -> u8 {
        let shard = self.shard(env_id);
        let Some((last, average, error_ratio)) =
            shard.get(&env_id).and_then(|load| load.compare(now / 60))
        else {
            return 0;
        };
        let mut bits = 0u8;
        if last.requests >= SPIKE_MIN_REQUESTS {
            let ratio = f64::from(last.requests) / average.max(1.0);
            for threshold in [3.0, 6.0, 12.0, 24.0] {
                if ratio >= threshold {
                    bits += 1;
                }
            }
        }
        if last.requests >= ERROR_MIN_REQUESTS {
            let last_ratio = f64::from(last.errors) / f64::from(last.requests);
            if last_ratio > 0.2 && last_ratio > 2.0 * error_ratio {
                bits += 1;
            }
        }
        bits.min(MAX_ADAPTIVE)
    }
}

/// Most visitors whose failures are remembered; past it the memory starts
/// over, which at worst gives a failing visitor the lower difficulty again.
const MAX_FAILING_VISITORS: usize = 100_000;

/// Failed solutions per visitor in the current hour, on this ingress.
#[derive(Default)]
pub struct FailureTracker {
    failures: Mutex<HashMap<VisitorIdentity, (u64, u8)>>,
}

impl FailureTracker {
    pub fn record(&self, identity: &VisitorIdentity, now: u64) {
        let hour = now / 3600;
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failures.len() >= MAX_FAILING_VISITORS && !failures.contains_key(identity) {
            failures.clear();
        }
        let entry = failures.entry(*identity).or_insert((hour, 0));
        if entry.0 != hour {
            *entry = (hour, 0);
        }
        entry.1 = entry.1.saturating_add(1);
    }

    /// Whether the visitor failed [`REPEATED_FAILURES`] times this hour.
    pub fn repeated(&self, identity: &VisitorIdentity, now: u64) -> bool {
        let failures = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        failures
            .get(identity)
            .is_some_and(|&(hour, count)| hour == now / 3600 && count >= REPEATED_FAILURES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000 - 1_800_000_000 % 60;

    #[test]
    fn the_base_follows_the_cause_and_the_cap_holds() {
        assert_eq!(difficulty(ChallengeCause::Verdict, false, 0), 18);
        assert_eq!(difficulty(ChallengeCause::Attack, false, 0), 20);
        assert_eq!(difficulty(ChallengeCause::BudgetSpent, false, 0), 20);
        assert_eq!(difficulty(ChallengeCause::Verdict, true, 0), 22);
        assert_eq!(difficulty(ChallengeCause::Verdict, false, 9), 22);
        assert_eq!(difficulty(ChallengeCause::Attack, true, 4), 24);
    }

    #[test]
    fn a_steady_environment_adds_nothing() {
        let load = LoadTracker::default();
        for minute in 0..61 {
            for _ in 0..200 {
                load.record_request(1, NOW + minute * 60);
            }
        }
        assert_eq!(load.extra_bits(1, NOW + 61 * 60), 0);
        assert_eq!(
            load.extra_bits(2, NOW),
            0,
            "unknown environments add nothing"
        );
    }

    #[test]
    fn a_spike_adds_bits_by_how_far_past_the_hour_it_is() {
        let load = LoadTracker::default();
        for minute in 0..59 {
            for _ in 0..50 {
                load.record_request(1, NOW + minute * 60);
            }
        }
        // 50 a minute for an hour, then 50 times that.
        for _ in 0..2_500 {
            load.record_request(1, NOW + 59 * 60);
        }
        assert_eq!(load.extra_bits(1, NOW + 60 * 60), 4);
        // A quiet environment's first busy minute is too small to count.
        let quiet = LoadTracker::default();
        for _ in 0..100 {
            quiet.record_request(3, NOW);
        }
        assert_eq!(quiet.extra_bits(3, NOW + 60), 0);
    }

    #[test]
    fn an_error_spike_adds_one() {
        let load = LoadTracker::default();
        for minute in 0..60 {
            for i in 0..100 {
                load.record_request(1, NOW + minute * 60);
                let failing = minute == 59 && i < 40;
                load.record_status(1, if failing { 503 } else { 200 }, NOW + minute * 60);
            }
        }
        assert_eq!(load.extra_bits(1, NOW + 60 * 60), 1);
    }

    #[test]
    fn repeated_failures_count_per_visitor_per_hour() {
        let failures = FailureTracker::default();
        let who = VisitorIdentity::compute(None, "ua", "none:direct");
        let other = VisitorIdentity::compute(None, "ua2", "none:direct");
        for _ in 0..REPEATED_FAILURES {
            assert!(!failures.repeated(&who, NOW));
            failures.record(&who, NOW);
        }
        assert!(failures.repeated(&who, NOW));
        assert!(!failures.repeated(&other, NOW));
        assert!(
            !failures.repeated(&who, NOW + 3600),
            "a new hour starts over"
        );
    }
}
