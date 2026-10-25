// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield (ADR 066), phase 1: proof-of-work challenges that the server signs
//! and verifies without a database.
//!
//! A challenge token binds the environment, the visitor identity (computed
//! from the connection, never from a request body), the difficulty and the
//! issue time. Solving it earns a clearance cookie bound to the same
//! environment and identity, valid for 24 hours, and limited to a number of
//! requests per hour on each ingress.

mod budget;
mod gate;
mod identity;
mod keys;
mod redeemed;
mod token;

pub use gate::{
    clearance_cookie_name, Admission, ChallengeGate, Clearance, CLEARANCE_COOKIE_PREFIX,
    DEFAULT_BUDGET_PER_HOUR,
};
pub use identity::VisitorIdentity;
pub use keys::ShieldKeys;
pub use token::{has_leading_zero_bits, ChallengeError, CHALLENGE_TTL_SECS, CLEARANCE_TTL_SECS};

/// Seconds since the Unix epoch, saturating at zero for a clock set before it.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
