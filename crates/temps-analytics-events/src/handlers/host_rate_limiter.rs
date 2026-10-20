// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lock-free per-project rate limiter for the Host-resolved (keyless) branch
//! of `POST /_temps/event`.
//!
//! This branch is the per-event hot path for every deployed site, so it must
//! not serialize traffic: no `Mutex`, no per-project allocation, no scan of a
//! timestamp history. The keyed branch keeps
//! [`temps_analytics::AnalyticsIngestRateLimiter`] (bounded by the number of
//! ingest keys); this one is a fixed-window counter per project held in a
//! fixed-size array of atomics.
//!
//! # Memory and accuracy
//!
//! [`SLOTS`] `AtomicU64`s (32 KiB), constant regardless of traffic or project
//! count. Each slot packs `project_id:32 | window:16 | count:16` and is updated
//! with a single compare-exchange loop.
//!
//! - **Fixed window**: a project can burst up to twice its limit across a
//!   window boundary. That is acceptable for an abuse ceiling that sits far
//!   above legitimate traffic.
//! - **Slot sharing**: project ids are SERIAL, so ids map to distinct slots
//!   until an install has more than [`SLOTS`] projects. Beyond that, two
//!   projects sharing a slot reset each other's count when they alternate.
//!   A collision can only *forget* history (admitting more), never reject a
//!   project for another project's traffic.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Number of counter slots (8 bytes each => 32 KiB).
const SLOTS: usize = 4_096;

/// Length of one counting window.
const WINDOW_SECS: u64 = 60;

const WINDOW_BITS: u32 = 16;
const COUNT_BITS: u32 = 16;
const WINDOW_MASK: u64 = (1 << WINDOW_BITS) - 1;
const COUNT_MASK: u64 = (1 << COUNT_BITS) - 1;

/// Per-project fixed-window limiter for Host-resolved analytics ingest.
pub struct HostIngestRateLimiter {
    started: Instant,
    slots: Box<[AtomicU64]>,
}

impl Default for HostIngestRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HostIngestRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostIngestRateLimiter")
            .field("slots", &SLOTS)
            .finish()
    }
}

impl HostIngestRateLimiter {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            slots: (0..SLOTS).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Count one event for `project_id` and report whether it is within
    /// `limit_per_minute`. A rejected event is not counted.
    pub fn check(&self, project_id: i32, limit_per_minute: u32) -> bool {
        self.check_at(
            project_id,
            limit_per_minute,
            self.started.elapsed().as_secs(),
        )
    }

    fn check_at(&self, project_id: i32, limit_per_minute: u32, now_secs: u64) -> bool {
        // The packed counter saturates at COUNT_MASK; a limit above that is
        // clamped rather than silently wrapping.
        let limit = u64::from(limit_per_minute).min(COUNT_MASK);
        let owner = u64::from(project_id as u32);
        let window = (now_secs / WINDOW_SECS) & WINDOW_MASK;
        let slot = &self.slots[(project_id as u32 as usize) % SLOTS];

        let mut current = slot.load(Ordering::Relaxed);
        loop {
            let same_project = current >> 32 == owner;
            let same_window = (current >> COUNT_BITS) & WINDOW_MASK == window;
            let count = if same_project && same_window {
                current & COUNT_MASK
            } else {
                0
            };
            if count >= limit {
                return false;
            }
            let next = (owner << 32) | (window << COUNT_BITS) | (count + 1);
            match slot.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_the_limit_then_rejects() {
        let limiter = HostIngestRateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check_at(7, 5, 0));
        }
        assert!(!limiter.check_at(7, 5, 0));
        assert!(!limiter.check_at(7, 5, 59));
    }

    #[test]
    fn a_new_window_restores_the_budget() {
        let limiter = HostIngestRateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check_at(7, 3, 10));
        }
        assert!(!limiter.check_at(7, 3, 10));
        assert!(limiter.check_at(7, 3, 10 + WINDOW_SECS));
    }

    #[test]
    fn projects_have_independent_budgets() {
        let limiter = HostIngestRateLimiter::new();
        assert!(limiter.check_at(1, 1, 0));
        assert!(!limiter.check_at(1, 1, 0));
        // A flood against project 1 never throttles project 2.
        assert!(limiter.check_at(2, 1, 0));
    }

    #[test]
    fn a_slot_collision_never_rejects_the_other_project() {
        let limiter = HostIngestRateLimiter::new();
        let a = 5;
        let b = 5 + SLOTS as i32;
        assert!(limiter.check_at(a, 1, 0));
        assert!(!limiter.check_at(a, 1, 0));
        // `b` shares `a`'s slot but is admitted: collisions only forget.
        assert!(limiter.check_at(b, 1, 0));
    }

    #[test]
    fn limits_above_the_packed_counter_are_clamped() {
        let limiter = HostIngestRateLimiter::new();
        for _ in 0..COUNT_MASK {
            assert!(limiter.check_at(9, u32::MAX, 0));
        }
        assert!(!limiter.check_at(9, u32::MAX, 0));
    }

    #[test]
    fn concurrent_callers_never_exceed_the_limit() {
        let limiter = std::sync::Arc::new(HostIngestRateLimiter::new());
        let admitted = std::sync::Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let limiter = limiter.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        if limiter.check_at(3, 2_500, 0) {
                            admitted.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("limiter thread should not panic");
        }
        assert_eq!(admitted.load(Ordering::Relaxed), 2_500);
    }
}
