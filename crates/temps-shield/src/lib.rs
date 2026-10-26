// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shield (ADR 066).
//!
//! Phase 1: proof-of-work challenges that the server signs and verifies
//! without a database.
//!
//! A challenge token binds the environment, the visitor identity (computed
//! from the connection, never from a request body), the difficulty and the
//! issue time. Solving it earns a clearance cookie bound to the same
//! environment and identity, valid for 24 hours, and limited to a number of
//! requests per hour on each ingress.
//!
//! Phase 2: reputation lists. The control plane fetches public lists and
//! compiles them into a [`ShieldSnapshot`]; each ingress loads it into an
//! [`Index`] and records the [`Verdict`] it gives every request.
//!
//! Phase 3: [`ja4`], the TLS client fingerprint each ingress computes from
//! the raw ClientHello.
//!
//! Phase 4 on: the [`ShieldEngine`] acts on verdicts per environment
//! ([`ShieldMode`]), with exemptions, [`bypass`] tokens, verified
//! [`crawlers`], adaptive [`difficulty`], exploit signatures and
//! [`browsers`] that are not what they claim, and records [`events`].

pub mod assets;
mod baseline;
pub mod browsers;
mod budget;
pub mod bypass;
pub mod crawlers;
pub mod difficulty;
mod engine;
pub mod events;
pub mod feeds;
mod gate;
mod identity;
mod index;
pub mod ja4;
mod keys;
mod net;
mod policy;
mod redeemed;
mod snapshot;
mod token;
mod verdict;

pub use baseline::IANA_RESERVED;
pub use engine::{
    evaluate, rejection_response, response_header_value, Admitted, Decision, Outcome, Request,
    ShieldEngine, VerifyOutcome, RESPONSE_HEADER,
};
pub use events::{EventKind, ShieldEvent};
pub use gate::{
    clearance_cookie_name, Admission, ChallengeGate, Clearance, CLEARANCE_COOKIE_PREFIX,
    DEFAULT_BUDGET_PER_HOUR,
};
pub use identity::{VisitorIdentity, VisitorNetwork};
pub use index::{Index, Lookup};
pub use keys::ShieldKeys;
pub use net::Network;
pub use policy::{EnvPolicy, ExemptPathError, ExemptPaths, ShieldMode, MAX_EXEMPT_PATHS};
pub use snapshot::{Ja4Entry, ScopedList, ShieldSnapshot, SnapshotError, SourceList};
pub use temps_signatures as signatures;
pub use token::{has_leading_zero_bits, ChallengeError, CHALLENGE_TTL_SECS, CLEARANCE_TTL_SECS};
pub use verdict::{Action, ListVerdict, Reason, Subject, Verdict};

/// Seconds since the Unix epoch, saturating at zero for a clock set before it.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
