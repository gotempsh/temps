// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lock-free fixed-window limiter for the unauthenticated flag endpoints
//! (ADR 034 R5.2: per-IP and per-project limits).
//!
//! Same design as the analytics ingest limiter
//! (`temps-analytics-events/src/handlers/host_rate_limiter.rs`): a fixed array
//! of atomics, each packing `owner:32 | window:16 | count:16`, updated by one
//! compare-exchange loop. Constant memory (32 KiB per limiter), no lock, no
//! allocation per request. Two owners sharing a slot reset each other's count
//! when they alternate, so a collision can only admit more, never reject a
//! caller for someone else's traffic.
//!
//! The owner is a project id for the per-project limiter and a 32-bit hash of
//! the client address for the per-IP one.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const SLOTS: usize = 4_096;
const WINDOW_SECS: u64 = 60;
const WINDOW_BITS: u32 = 16;
const COUNT_BITS: u32 = 16;
const WINDOW_MASK: u64 = (1 << WINDOW_BITS) - 1;
const COUNT_MASK: u64 = (1 << COUNT_BITS) - 1;

pub struct SlotRateLimiter {
    started: Instant,
    slots: Box<[AtomicU64]>,
}

impl Default for SlotRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotRateLimiter {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            slots: (0..SLOTS).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Count one request for `owner`; `false` when it is over the limit. A
    /// rejected request is not counted.
    pub fn check(&self, owner: u32, limit_per_minute: u32) -> bool {
        self.check_at(owner, limit_per_minute, self.started.elapsed().as_secs())
    }

    /// The owner key for a client address string.
    pub fn owner_for_address(address: &str) -> u32 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        address.hash(&mut hasher);
        let hash = hasher.finish();
        (hash ^ (hash >> 32)) as u32
    }

    fn check_at(&self, owner: u32, limit_per_minute: u32, now_secs: u64) -> bool {
        let limit = u64::from(limit_per_minute).min(COUNT_MASK);
        let owner_bits = u64::from(owner);
        let window = (now_secs / WINDOW_SECS) & WINDOW_MASK;
        let slot = &self.slots[(owner as usize) % SLOTS];

        let mut current = slot.load(Ordering::Relaxed);
        loop {
            let same_owner = current >> 32 == owner_bits;
            let same_window = (current >> COUNT_BITS) & WINDOW_MASK == window;
            let count = if same_owner && same_window {
                current & COUNT_MASK
            } else {
                0
            };
            if count >= limit {
                return false;
            }
            let next = (owner_bits << 32) | (window << COUNT_BITS) | (count + 1);
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
        let limiter = SlotRateLimiter::new();
        for _ in 0..5 {
            assert!(limiter.check_at(7, 5, 0));
        }
        assert!(!limiter.check_at(7, 5, 0));
        assert!(
            limiter.check_at(7, 5, WINDOW_SECS),
            "a new window restores the budget"
        );
    }

    #[test]
    fn owners_have_independent_budgets() {
        let limiter = SlotRateLimiter::new();
        assert!(limiter.check_at(1, 1, 0));
        assert!(!limiter.check_at(1, 1, 0));
        assert!(limiter.check_at(2, 1, 0));
    }

    #[test]
    fn addresses_map_to_stable_owners() {
        assert_eq!(
            SlotRateLimiter::owner_for_address("203.0.113.7"),
            SlotRateLimiter::owner_for_address("203.0.113.7")
        );
        assert_ne!(
            SlotRateLimiter::owner_for_address("203.0.113.7"),
            SlotRateLimiter::owner_for_address("203.0.113.8")
        );
    }
}
