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
//! Each network may hold only `per_network` of the salts, so one client
//! solving challenges as fast as it can fills its own share, not the set
//! everyone else needs.
//!
//! Only tokens that passed the signature, visitor and proof-of-work checks
//! get here, so the lock is taken once per solved challenge, not per
//! request.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use crate::identity::VisitorNetwork;
use crate::token::CHALLENGE_TTL_SECS;

const SECONDS_PER_GROUP: u64 = 60;

/// Why a salt was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotRecorded {
    /// The salt was already redeemed.
    Replayed,
    /// The visitor's network holds `per_network` unexpired salts.
    NetworkFull,
    /// The set holds `capacity` unexpired salts.
    Full,
}

pub(crate) struct RedeemedSalts {
    capacity: usize,
    per_network: u32,
    inner: Mutex<Groups>,
}

#[derive(Default)]
struct Groups {
    by_minute: BTreeMap<u64, Group>,
    /// Unexpired salts per network, across all groups.
    per_network: HashMap<VisitorNetwork, u32>,
    len: usize,
}

#[derive(Default)]
struct Group {
    salts: HashSet<[u8; 16]>,
    per_network: HashMap<VisitorNetwork, u32>,
}

impl RedeemedSalts {
    pub(crate) fn new(capacity: usize, per_network: u32) -> Self {
        Self {
            capacity,
            per_network,
            inner: Mutex::new(Groups::default()),
        }
    }

    /// Records `salt` for a token issued at `issued_at` and redeemed from
    /// `network`, unless it is already there or the network's share or the
    /// whole set is full of unexpired salts. Checking and inserting happen
    /// under one lock, so of several concurrent calls with one salt exactly
    /// one succeeds.
    pub(crate) fn record(
        &self,
        salt: [u8; 16],
        issued_at: u64,
        network: VisitorNetwork,
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
            .is_some_and(|group| group.salts.contains(&salt))
        {
            return Err(NotRecorded::Replayed);
        }
        if groups.per_network.get(&network).copied().unwrap_or(0) >= self.per_network {
            return Err(NotRecorded::NetworkFull);
        }
        if groups.len >= self.capacity {
            return Err(NotRecorded::Full);
        }
        let group = groups.by_minute.entry(minute).or_default();
        group.salts.insert(salt);
        *group.per_network.entry(network).or_default() += 1;
        *groups.per_network.entry(network).or_default() += 1;
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
            let group = entry.remove();
            self.len -= group.salts.len();
            for (network, count) in group.per_network {
                if let Some(total) = self.per_network.get_mut(&network) {
                    *total -= count;
                    if *total == 0 {
                        self.per_network.remove(&network);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;
    const NET: VisitorNetwork = VisitorNetwork([4, 203, 0, 113, 0, 0, 0, 0]);
    const OTHER_NET: VisitorNetwork = VisitorNetwork([4, 198, 51, 100, 0, 0, 0, 0]);

    #[test]
    fn a_salt_is_recorded_once() {
        let salts = RedeemedSalts::new(10, 10);
        assert_eq!(salts.record([1; 16], NOW, NET, NOW), Ok(()));
        assert_eq!(
            salts.record([1; 16], NOW, NET, NOW + 10),
            Err(NotRecorded::Replayed)
        );
        assert_eq!(salts.record([2; 16], NOW, NET, NOW + 10), Ok(()));
    }

    #[test]
    fn a_salt_is_kept_while_its_token_is_valid() {
        let salts = RedeemedSalts::new(10, 10);
        salts.record([1; 16], NOW, NET, NOW).ok();
        assert_eq!(
            salts.record([1; 16], NOW, NET, NOW + CHALLENGE_TTL_SECS - 1),
            Err(NotRecorded::Replayed)
        );
    }

    #[test]
    fn a_full_set_refuses_until_its_tokens_expire() {
        let salts = RedeemedSalts::new(2, 10);
        salts.record([1; 16], NOW, NET, NOW).ok();
        salts.record([2; 16], NOW, NET, NOW).ok();
        assert_eq!(salts.record([3; 16], NOW, NET, NOW), Err(NotRecorded::Full));
        // A replay is still a replay when the set is full.
        assert_eq!(
            salts.record([1; 16], NOW, NET, NOW),
            Err(NotRecorded::Replayed)
        );
        let later = NOW + SECONDS_PER_GROUP + CHALLENGE_TTL_SECS;
        assert_eq!(salts.record([3; 16], later, NET, later), Ok(()));
        assert_eq!(salts.record([4; 16], later, NET, later), Ok(()));
    }

    #[test]
    fn one_network_cannot_take_the_whole_set() {
        let salts = RedeemedSalts::new(10, 2);
        salts.record([1; 16], NOW, NET, NOW).ok();
        salts.record([2; 16], NOW, NET, NOW).ok();
        assert_eq!(
            salts.record([3; 16], NOW, NET, NOW),
            Err(NotRecorded::NetworkFull)
        );
        assert_eq!(salts.record([3; 16], NOW, OTHER_NET, NOW), Ok(()));
        // The network's share comes back when its tokens expire.
        let later = NOW + SECONDS_PER_GROUP + CHALLENGE_TTL_SECS;
        assert_eq!(salts.record([4; 16], later, NET, later), Ok(()));
        assert_eq!(salts.record([5; 16], later, NET, later), Ok(()));
    }
}
