// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield at the proxy (ADR 066).
//!
//! Every request to a project goes through one [`temps_shield::ShieldEngine`]
//! decision ([`ProxyShield`]), with the reputation lists ([`ShieldLists`],
//! fetched and compiled by [`ShieldSources`]), the environment's mode,
//! exempt paths and [`BypassTokens`]. Decisions other than `pass` are
//! recorded through the [`ShieldEventStore`], and [`ShieldLearning`]
//! promotes what the fleet's events show (D9) and prunes the Shield tables.
//!
//! The proxy issues a signed proof-of-work token with the challenge page,
//! verifies the solution itself at [`VERIFY_PATH`], and admits later
//! requests on a signed clearance cookie. The visitor identity both are
//! bound to is computed from the connection; nothing the client sends in a
//! body decides who it is or which environment it cleared.

mod bypass;
pub mod event_store;
mod ingress;
mod ja4_hook;
mod learning;
mod lists;
mod sources;

pub use bypass::{BypassTokens, POLL_INTERVAL as BYPASS_POLL_INTERVAL};
pub use event_store::{EventBackend, ShieldEventStore, ShieldEventWriter};
pub use ingress::{decision_event, verify_event, EventContext, ProxyShield, CRAWLER_CHECK_TIMEOUT};
pub use ja4_hook::Ja4Hook;
pub use learning::{EvidenceStore, LearningError, Promoted, Pruned, ShieldLearning};
pub use lists::{ShieldLists, ShieldListsError, POLL_INTERVAL};
pub use sources::{Fetched, ShieldSources, SourceError, OBSERVED_SOURCE, OPERATOR_SOURCE};

use temps_shield::{ChallengeGate, ShieldKeys};

pub use temps_shield::assets::{challenge_page, MAX_VERIFY_BODY, VERIFY_PATH};

/// The challenge gate for this process, keyed from the master key so every
/// ingress that holds it signs and verifies the same tokens.
pub fn challenge_gate(encryption: &temps_core::EncryptionService) -> ChallengeGate {
    ChallengeGate::new(ShieldKeys::derive(|label| encryption.derive_subkey(label)))
}

#[cfg(test)]
pub(crate) fn test_gate() -> ChallengeGate {
    ChallengeGate::new(ShieldKeys::derive(|label| {
        let mut key = [0u8; 32];
        for (slot, byte) in key.iter_mut().zip(label.bytes().cycle()) {
            *slot = byte;
        }
        key
    }))
}
