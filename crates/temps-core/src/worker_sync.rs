// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Timing of a worker's route and DNS sync clients.
//!
//! The deployment completion gate reverts a deployment when a worker's ACK does
//! not arrive in time, so the gate has to know how long these clients can take.
//! They live here so the clients and the gate read the same numbers.

/// Cap on the retry backoff of both sync clients.
pub const SYNC_MAX_BACKOFF_SECS: u64 = 30;

/// Timeout of each DNS sync request. `temps_node_dns::ResolverConfig` repeats
/// this default, because that crate can be built without this one; a test there
/// fails if the two differ.
pub const DNS_SYNC_REQUEST_TIMEOUT_SECS: u64 = 10;

/// Timeout of a route snapshot request. This has to exceed the control plane's
/// 25s long-poll, but a snapshot for a generation the worker has not seen is
/// answered immediately.
pub const ROUTE_SNAPSHOT_TIMEOUT_SECS: u64 = 60;

/// Timeout of a route ACK, which the control plane never holds open.
pub const ROUTE_ACK_TIMEOUT_SECS: u64 = 10;

/// A DNS round: fetch the changes, then ACK them.
const DNS_ROUND_SECS: u64 = 2 * DNS_SYNC_REQUEST_TIMEOUT_SECS;

/// A route round: resend an outstanding ACK, fetch the snapshot, then ACK it.
const ROUTE_ROUND_SECS: u64 =
    ROUTE_ACK_TIMEOUT_SECS + ROUTE_SNAPSHOT_TIMEOUT_SECS + ROUTE_ACK_TIMEOUT_SECS;

/// Longest a worker takes to ACK from the moment it starts its longest retry
/// backoff, when every request of the next round succeeds just inside its
/// timeout. A worker that fails that round too is not recovering, and is not
/// covered.
pub const WORKER_SYNC_RECOVERY_SECS: u64 = SYNC_MAX_BACKOFF_SECS
    + if ROUTE_ROUND_SECS > DNS_ROUND_SECS {
        ROUTE_ROUND_SECS
    } else {
        DNS_ROUND_SECS
    };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_covers_a_full_round_of_the_slowest_client() {
        assert_eq!(SYNC_MAX_BACKOFF_SECS + DNS_ROUND_SECS, 50);
        assert_eq!(SYNC_MAX_BACKOFF_SECS + ROUTE_ROUND_SECS, 110);
        assert_eq!(WORKER_SYNC_RECOVERY_SECS, 110);
    }
}
