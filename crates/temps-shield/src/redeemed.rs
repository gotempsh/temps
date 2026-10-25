// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The salts of tokens already redeemed on this ingress.
//!
//! Salts are grouped by the minute their token was issued, and a group is
//! dropped once every token in it has expired, so a salt is never forgotten
//! while its token could still be redeemed. Expiry follows the `now` the
//! caller passes, like every other check in this crate, so capacity frees
//! up as soon as tokens expire.
//!
//! Only tokens that passed the signature, visitor and proof-of-work checks
//! get here, so the lock is taken once per solved challenge, not per
//! request.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, PoisonError};

use crate::token::CHALLENGE_TTL_SECS;

const SECONDS_PER_GROUP: u64 = 60;

/// Why a salt was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotRecorded {
    /// The salt was already redeemed.
    Replayed,
    /// The set holds `capacity` unexpired salts.
    Full,
}

pub(crate) struct RedeemedSalts {
    capacity: usize,
    inner: Mutex<Groups>,
}

#[derive(Default)]
struct Groups {
    by_minute: BTreeMap<u64, HashSet<[u8; 16]>>,
    len: usize,
}

impl RedeemedSalts {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(Groups::default()),
        }
    }

    /// Records `salt` for a token issued at `issued_at`, unless it is
    /// already there or the set is full of unexpired salts. Checking and
    /// inserting happen under one lock, so of several concurrent calls with
    /// one salt exactly one succeeds.
    pub(crate) fn record(
        &self,
        salt: [u8; 16],
        issued_at: u64,
        now: u64,
    ) -> Result<(), NotRecorded> {
        // A panic while holding the lock leaves the groups consistent: each
        // step below is a single collection call.
        let mut groups = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        groups.drop_expired(now);
        let minute = issued_at / SECONDS_PER_GROUP;
        if groups
            .by_minute
            .get(&minute)
            .is_some_and(|group| group.contains(&salt))
        {
            return Err(NotRecorded::Replayed);
        }
        if groups.len >= self.capacity {
            return Err(NotRecorded::Full);
        }
        groups.by_minute.entry(minute).or_default().insert(salt);
        groups.len += 1;
        Ok(())
    }
}

impl Groups {
    /// Drops every group whose tokens have all expired: a token issued in
    /// minute `m` is redeemable until `(m + 1) * 60 + CHALLENGE_TTL_SECS`.
    fn drop_expired(&mut self, now: u64) {
        while let Some(entry) = self.by_minute.first_entry() {
            let last_valid = (entry.key() + 1)
                .saturating_mul(SECONDS_PER_GROUP)
                .saturating_add(CHALLENGE_TTL_SECS);
            if now < last_valid {
                break;
            }
            self.len -= entry.remove().len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn a_salt_is_recorded_once() {
        let salts = RedeemedSalts::new(10);
        assert_eq!(salts.record([1; 16], NOW, NOW), Ok(()));
        assert_eq!(
            salts.record([1; 16], NOW, NOW + 10),
            Err(NotRecorded::Replayed)
        );
        assert_eq!(salts.record([2; 16], NOW, NOW + 10), Ok(()));
    }

    #[test]
    fn a_salt_is_kept_while_its_token_is_valid() {
        let salts = RedeemedSalts::new(10);
        salts.record([1; 16], NOW, NOW).ok();
        assert_eq!(
            salts.record([1; 16], NOW, NOW + CHALLENGE_TTL_SECS - 1),
            Err(NotRecorded::Replayed)
        );
    }

    #[test]
    fn a_full_set_refuses_until_its_tokens_expire() {
        let salts = RedeemedSalts::new(2);
        salts.record([1; 16], NOW, NOW).ok();
        salts.record([2; 16], NOW, NOW).ok();
        assert_eq!(salts.record([3; 16], NOW, NOW), Err(NotRecorded::Full));
        // A replay is still a replay when the set is full.
        assert_eq!(salts.record([1; 16], NOW, NOW), Err(NotRecorded::Replayed));
        let later = NOW + SECONDS_PER_GROUP + CHALLENGE_TTL_SECS;
        assert_eq!(salts.record([3; 16], later, later), Ok(()));
        assert_eq!(salts.record([4; 16], later, later), Ok(()));
    }
}
