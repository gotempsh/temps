// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Admission control for the public tracking-pixel / click-redirect endpoints.
//!
//! A tracking URL is HMAC-bound to its email (and, for clicks, its target
//! URL), but it is not single-use: anyone holding a copy can replay it
//! indefinitely. Before this guard every hit spawned an unbounded background
//! task doing an IP-geolocation lookup and a DB insert, so flooding one
//! known-good URL inflated the email's open/click metrics and grew the task
//! set and DB write rate without bound.
//!
//! Each hit that passed HMAC verification now goes through three checks, in
//! order, before any work is spawned:
//!
//! 1. **Replay dedupe** per `(email_id, event kind, client ip)`: a repeat
//!    inside [`DEDUPE_WINDOW_SECS`] is a replay (or a mail client prefetching
//!    the same image twice) and is not recorded again.
//! 2. **Per-email cap** per `(email_id, event kind)`: at most
//!    [`PER_EMAIL_MAX_EVENTS_PER_WINDOW`] events in a [`PER_EMAIL_WINDOW_SECS`]
//!    window, whatever the client IP. This is what holds when the caller
//!    rotates the (client-supplied) `X-Forwarded-For` value to defeat (1).
//! 3. **Concurrency bound**: at most [`MAX_IN_FLIGHT_RECORDS`] record tasks in
//!    flight process-wide, acquired with `try_acquire_owned` — a saturated
//!    pool drops the event, it never queues or blocks the response.
//!
//! The HTTP response (the GIF, or the redirect) is returned in every case;
//! only the bookkeeping is dropped. Drops are counted and logged at a
//! throttled rate.
//!
//! # Memory and locking
//!
//! Both tables are fixed-size arrays of `AtomicU64` slots indexed by a
//! per-process randomly keyed SipHash (so slot collisions cannot be planned
//! by a caller): ~160 KiB total, constant regardless of traffic, and no lock
//! on the request path. A slot collision can only *forget* history (making a
//! duplicate look new, or resetting a counter), never invent a duplicate,
//! except on a 32-bit fingerprint match, which is negligible.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;
use uuid::Uuid;

/// A repeat of the same `(email, kind, ip)` within this many seconds is not
/// recorded again.
pub const DEDUPE_WINDOW_SECS: u64 = 60;

/// Length of the per-email counting window.
pub const PER_EMAIL_WINDOW_SECS: u64 = 60;

/// Maximum recorded events of one kind for one email per window, across all
/// client IPs. A real recipient (or a handful of forwarded copies) stays far
/// below this; a replay flood is capped at it.
pub const PER_EMAIL_MAX_EVENTS_PER_WINDOW: u64 = 20;

/// Maximum concurrent record-event tasks (geolocation + insert) for the
/// whole process.
pub const MAX_IN_FLIGHT_RECORDS: usize = 64;

/// Number of replay-dedupe slots (8 bytes each => 128 KiB).
const DEDUPE_SLOTS: usize = 16_384;

/// Number of per-email counter slots (8 bytes each => 32 KiB).
const EMAIL_SLOTS: usize = 4_096;

/// Per-email slot layout: `fingerprint:32 | window:20 | count:12`.
const WINDOW_BITS: u32 = 20;
const COUNT_BITS: u32 = 12;
const WINDOW_MASK: u64 = (1 << WINDOW_BITS) - 1;
const COUNT_MASK: u64 = (1 << COUNT_BITS) - 1;

// The packed counter must be able to represent the cap.
const _: () = assert!(PER_EMAIL_MAX_EVENTS_PER_WINDOW <= COUNT_MASK);

/// The kind of tracking hit, part of every dedupe/cap key so an open never
/// suppresses a click (or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackingHitKind {
    Open,
    Click,
}

impl TrackingHitKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Click => "click",
        }
    }
}

/// Why a hit was not admitted for recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingHitRejection {
    /// Same `(email, kind, ip)` seen within [`DEDUPE_WINDOW_SECS`].
    Duplicate,
    /// The email exceeded [`PER_EMAIL_MAX_EVENTS_PER_WINDOW`] for this kind.
    RateLimited,
    /// [`MAX_IN_FLIGHT_RECORDS`] record tasks are already running.
    Saturated,
}

/// Lock-free, constant-memory admission control for tracking hits. See the
/// module docs.
pub struct TrackingHitGuard {
    started: Instant,
    hasher: RandomState,
    dedupe: Box<[AtomicU64]>,
    per_email: Box<[AtomicU64]>,
    in_flight: Arc<Semaphore>,
    dropped_duplicate: AtomicU64,
    dropped_rate_limited: AtomicU64,
    dropped_saturated: AtomicU64,
}

impl Default for TrackingHitGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl TrackingHitGuard {
    pub fn new() -> Self {
        Self::with_capacity(MAX_IN_FLIGHT_RECORDS)
    }

    fn with_capacity(max_in_flight: usize) -> Self {
        Self {
            started: Instant::now(),
            hasher: RandomState::new(),
            dedupe: (0..DEDUPE_SLOTS).map(|_| AtomicU64::new(0)).collect(),
            per_email: (0..EMAIL_SLOTS).map(|_| AtomicU64::new(0)).collect(),
            in_flight: Arc::new(Semaphore::new(max_in_flight)),
            dropped_duplicate: AtomicU64::new(0),
            dropped_rate_limited: AtomicU64::new(0),
            dropped_saturated: AtomicU64::new(0),
        }
    }

    /// Decide whether a verified hit should be recorded. On success, returns
    /// the permit that must be held by the spawned record task for its whole
    /// lifetime (it bounds concurrency when dropped).
    ///
    /// `client_ip` is whatever the handler resolved; an unparseable or absent
    /// value shares one "unknown" dedupe identity per email.
    pub fn admit(
        &self,
        email_id: Uuid,
        kind: TrackingHitKind,
        client_ip: Option<&str>,
    ) -> Result<OwnedSemaphorePermit, TrackingHitRejection> {
        let now = self.started.elapsed().as_secs();
        self.admit_at(email_id, kind, client_ip, now)
    }

    fn admit_at(
        &self,
        email_id: Uuid,
        kind: TrackingHitKind,
        client_ip: Option<&str>,
        now_secs: u64,
    ) -> Result<OwnedSemaphorePermit, TrackingHitRejection> {
        // Normalise so "1.2.3.4" and " 1.2.3.4" (or IPv6 spellings) dedupe
        // together; anything unparseable collapses to `None`.
        let ip: Option<IpAddr> = client_ip.and_then(|s| s.trim().parse().ok());

        let (reason, counter) = if self.is_duplicate(email_id, kind, ip, now_secs) {
            (TrackingHitRejection::Duplicate, &self.dropped_duplicate)
        } else if !self.within_per_email_cap(email_id, kind, now_secs) {
            (
                TrackingHitRejection::RateLimited,
                &self.dropped_rate_limited,
            )
        } else {
            match self.in_flight.clone().try_acquire_owned() {
                Ok(permit) => return Ok(permit),
                Err(_) => (TrackingHitRejection::Saturated, &self.dropped_saturated),
            }
        };

        let total = counter.fetch_add(1, Ordering::Relaxed) + 1;
        // Throttled: a flood must not turn into a log flood.
        if total == 1 || total % 1_000 == 0 {
            warn!(
                email_id = %email_id,
                kind = kind.as_str(),
                reason = ?reason,
                dropped_total = total,
                "Email tracking hit not recorded (replay/rate/concurrency guard); \
                 response still served"
            );
        }
        Err(reason)
    }

    /// Records the `(email, kind, ip)` hit and reports whether the same key
    /// was already seen within [`DEDUPE_WINDOW_SECS`]. A duplicate refreshes
    /// the slot, so a steady replay every < window seconds stays suppressed.
    fn is_duplicate(
        &self,
        email_id: Uuid,
        kind: TrackingHitKind,
        ip: Option<IpAddr>,
        now_secs: u64,
    ) -> bool {
        let hash = self.hasher.hash_one((email_id, kind, ip));
        let slot = &self.dedupe[(hash as usize) % DEDUPE_SLOTS];
        // Non-zero fingerprint so an empty slot (0) never matches.
        let fingerprint = ((hash >> 32) as u32 | 1) as u64;
        // Store `now + 1` so 0 stays the "empty" marker; truncation to 32 bits
        // only wraps after ~136 years of uptime.
        let stamp = (now_secs.wrapping_add(1)) & 0xFFFF_FFFF;
        let previous = slot.swap((fingerprint << 32) | stamp, Ordering::Relaxed);

        if previous == 0 || previous >> 32 != fingerprint {
            return false;
        }
        let previous_stamp = previous & 0xFFFF_FFFF;
        stamp.wrapping_sub(previous_stamp) & 0xFFFF_FFFF < DEDUPE_WINDOW_SECS
    }

    /// Counts one event against the `(email, kind)` window and reports
    /// whether it is within [`PER_EMAIL_MAX_EVENTS_PER_WINDOW`].
    fn within_per_email_cap(&self, email_id: Uuid, kind: TrackingHitKind, now_secs: u64) -> bool {
        let hash = self.hasher.hash_one((email_id, kind));
        let slot = &self.per_email[(hash as usize) % EMAIL_SLOTS];
        let fingerprint = ((hash >> 32) as u32 | 1) as u64;
        let window = (now_secs / PER_EMAIL_WINDOW_SECS) & WINDOW_MASK;

        let mut current = slot.load(Ordering::Relaxed);
        loop {
            let same_key = current >> 32 == fingerprint;
            let same_window = (current >> COUNT_BITS) & WINDOW_MASK == window;
            let count = if same_key && same_window {
                current & COUNT_MASK
            } else {
                0
            };
            if count >= PER_EMAIL_MAX_EVENTS_PER_WINDOW {
                return false;
            }
            let next = (fingerprint << 32) | (window << COUNT_BITS) | (count + 1);
            match slot.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Total hits not recorded, by reason: `(duplicate, rate_limited, saturated)`.
    pub fn dropped_counts(&self) -> (u64, u64, u64) {
        (
            self.dropped_duplicate.load(Ordering::Relaxed),
            self.dropped_rate_limited.load(Ordering::Relaxed),
            self.dropped_saturated.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> Uuid {
        Uuid::new_v4()
    }

    #[tokio::test]
    async fn first_hit_is_admitted() {
        let guard = TrackingHitGuard::new();
        assert!(guard
            .admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0)
            .is_ok());
    }

    #[tokio::test]
    async fn replay_from_same_ip_within_window_is_deduped() {
        let guard = TrackingHitGuard::new();
        let id = email();
        let ip = Some("192.0.2.1");
        assert!(guard.admit_at(id, TrackingHitKind::Open, ip, 10).is_ok());
        assert_eq!(
            guard.admit_at(id, TrackingHitKind::Open, ip, 11).err(),
            Some(TrackingHitRejection::Duplicate)
        );
        // Whitespace / formatting differences are the same client.
        assert_eq!(
            guard
                .admit_at(id, TrackingHitKind::Open, Some(" 192.0.2.1 "), 12)
                .err(),
            Some(TrackingHitRejection::Duplicate)
        );
        assert_eq!(guard.dropped_counts().0, 2);
    }

    #[tokio::test]
    async fn same_ip_after_window_is_admitted_again() {
        let guard = TrackingHitGuard::new();
        let id = email();
        let ip = Some("192.0.2.1");
        assert!(guard.admit_at(id, TrackingHitKind::Open, ip, 0).is_ok());
        assert!(guard
            .admit_at(id, TrackingHitKind::Open, ip, DEDUPE_WINDOW_SECS + 1)
            .is_ok());
    }

    #[tokio::test]
    async fn open_and_click_and_other_ips_are_independent() {
        let guard = TrackingHitGuard::new();
        let id = email();
        assert!(guard
            .admit_at(id, TrackingHitKind::Open, Some("192.0.2.1"), 0)
            .is_ok());
        assert!(guard
            .admit_at(id, TrackingHitKind::Click, Some("192.0.2.1"), 0)
            .is_ok());
        assert!(guard
            .admit_at(id, TrackingHitKind::Open, Some("192.0.2.2"), 0)
            .is_ok());
        assert!(guard
            .admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0)
            .is_ok());
    }

    #[tokio::test]
    async fn rotating_ips_is_capped_per_email() {
        let guard = TrackingHitGuard::new();
        let id = email();
        let mut admitted = 0;
        let mut rate_limited = 0;
        for i in 0..200u32 {
            let ip = format!("10.0.{}.{}", i / 256, i % 256);
            match guard.admit_at(id, TrackingHitKind::Click, Some(&ip), 5) {
                Ok(_) => admitted += 1,
                Err(TrackingHitRejection::RateLimited) => rate_limited += 1,
                Err(other) => panic!("unexpected rejection {:?}", other),
            }
        }
        assert_eq!(admitted, PER_EMAIL_MAX_EVENTS_PER_WINDOW);
        assert_eq!(rate_limited, 200 - PER_EMAIL_MAX_EVENTS_PER_WINDOW);

        // A new window restores the budget.
        assert!(guard
            .admit_at(
                id,
                TrackingHitKind::Click,
                Some("10.9.9.9"),
                5 + PER_EMAIL_WINDOW_SECS
            )
            .is_ok());
    }

    #[tokio::test]
    async fn saturated_pool_drops_instead_of_spawning() {
        let guard = TrackingHitGuard::with_capacity(2);
        let p1 = guard.admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0);
        let p2 = guard.admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0);
        assert!(p1.is_ok() && p2.is_ok());
        assert_eq!(
            guard
                .admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0)
                .err(),
            Some(TrackingHitRejection::Saturated)
        );
        // Finishing a task (dropping its permit) frees a slot.
        drop(p1);
        assert!(guard
            .admit_at(email(), TrackingHitKind::Open, Some("192.0.2.1"), 0)
            .is_ok());
        assert_eq!(guard.dropped_counts().2, 1);
    }

    #[tokio::test]
    async fn missing_or_garbage_ip_shares_one_identity() {
        let guard = TrackingHitGuard::new();
        let id = email();
        assert!(guard.admit_at(id, TrackingHitKind::Open, None, 0).is_ok());
        assert_eq!(
            guard
                .admit_at(id, TrackingHitKind::Open, Some("not-an-ip"), 1)
                .err(),
            Some(TrackingHitRejection::Duplicate)
        );
    }
}
