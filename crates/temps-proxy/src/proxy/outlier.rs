// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Passive outlier detection: what a request learns about its replica when
//! it connects, fed back into replica selection so a replica that refuses or
//! times out connections leaves rotation before the active health probe
//! notices it.
//!
//! Both hooks run once per new upstream connection, touch only the
//! replica's atomics and allocate nothing unless a replica is ejected.

use pingora::{Error, ErrorType};
use temps_routes::replica_policy::{ConnectFailureOutcome, InFlightGuard};
use tracing::warn;

/// A new connection to the replica could not be made at all. TLS and
/// socket-setup errors are left out: they say nothing about whether the
/// replica is up.
fn is_replica_down(error: &Error) -> bool {
    matches!(
        error.etype(),
        ErrorType::ConnectRefused | ErrorType::ConnectTimedout | ErrorType::ConnectNoRoute
    )
}

/// Called from `fail_to_connect` with the replica the request was given.
pub(super) fn record_connect_failure(
    replica: Option<&InFlightGuard>,
    peer_address: &impl std::fmt::Display,
    error: &Error,
) -> Option<ConnectFailureOutcome> {
    let replica = replica?;
    if !is_replica_down(error) {
        return None;
    }
    let outcome = replica.state().record_connect_failure();
    if let ConnectFailureOutcome::Ejected(backoff) = outcome {
        warn!(
            upstream = %peer_address,
            ejected_for_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
            error = ?error.etype(),
            "Replica taken out of rotation after failed connects; it gets one request again when the ejection runs out"
        );
    }
    Some(outcome)
}

/// Called from `connected_to_upstream`. A reused pooled connection proves
/// nothing about the replica accepting new ones, so only a new one counts.
pub(super) fn record_connect_success(replica: Option<&InFlightGuard>, reused: bool) {
    if let (Some(replica), false) = (replica, reused) {
        replica.state().record_connect_success();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use temps_routes::replica_policy::{ReplicaState, OUTLIER_CONSECUTIVE_CONNECT_FAILURES};

    fn refused() -> Box<Error> {
        Error::new(ErrorType::ConnectRefused)
    }

    #[test]
    fn refused_connects_take_the_replica_out_of_rotation() {
        let state = Arc::new(ReplicaState::default());
        let mut last = None;
        for _ in 0..OUTLIER_CONSECUTIVE_CONNECT_FAILURES {
            let guard = InFlightGuard::acquire(state.clone());
            last = record_connect_failure(Some(&guard), &"10.0.0.2:3000", &refused());
        }
        assert!(matches!(last, Some(ConnectFailureOutcome::Ejected(_))));
        assert!(state.out_of_rotation());
    }

    #[test]
    fn timeouts_and_unroutable_replicas_count_but_tls_errors_do_not() {
        let state = Arc::new(ReplicaState::default());
        let guard = InFlightGuard::acquire(state.clone());
        for _ in 0..OUTLIER_CONSECUTIVE_CONNECT_FAILURES * 2 {
            assert_eq!(
                record_connect_failure(
                    Some(&guard),
                    &"10.0.0.2:3000",
                    &Error::new(ErrorType::TLSHandshakeFailure)
                ),
                None
            );
        }
        assert!(!state.out_of_rotation());
        record_connect_failure(
            Some(&guard),
            &"10.0.0.2:3000",
            &Error::new(ErrorType::ConnectTimedout),
        );
        record_connect_failure(
            Some(&guard),
            &"10.0.0.2:3000",
            &Error::new(ErrorType::ConnectNoRoute),
        );
        record_connect_failure(Some(&guard), &"10.0.0.2:3000", &refused());
        assert!(state.out_of_rotation());
    }

    #[test]
    fn only_a_new_connection_clears_the_failures() {
        let state = Arc::new(ReplicaState::default());
        let guard = InFlightGuard::acquire(state.clone());
        for _ in 1..OUTLIER_CONSECUTIVE_CONNECT_FAILURES {
            record_connect_failure(Some(&guard), &"10.0.0.2:3000", &refused());
        }
        record_connect_success(Some(&guard), true);
        record_connect_failure(Some(&guard), &"10.0.0.2:3000", &refused());
        assert!(state.out_of_rotation(), "a reused connection reset nothing");

        record_connect_success(Some(&guard), false);
        assert!(!state.out_of_rotation());
    }

    #[test]
    fn a_request_without_a_replica_records_nothing() {
        assert_eq!(
            record_connect_failure(None, &"10.0.0.2:3000", &refused()),
            None
        );
        record_connect_success(None, false);
    }
}
